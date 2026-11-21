//! The HTTP application: router, middleware and the health endpoint.
//!
//! Layer order, outermost first: request logging, protocol version check,
//! error-body normalisation, body size limit, routes.

use axum::body::Body;
use axum::extract::{MatchedPath, Request, State};
use axum::http::{HeaderName, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use core_proto::{ErrorBody, ErrorCode, HEADER_PROTOCOL, PROTOCOL_VERSION};
use serde::{Deserialize, Serialize};
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::trace::TraceLayer;
use tracing::field::Empty;

use crate::error::{ApiError, code_for_status};
use crate::state::AppState;
use crate::util::{hex, random_bytes};

/// Oldest client protocol major this server still serves.
pub const MIN_CLIENT_PROTOCOL: u32 = 1;

/// Body of `GET /v1/health`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthResp {
    /// Always `"ok"` when the server answers.
    pub status: String,
    /// The server's protocol major.
    pub protocol: u32,
    /// Database identity; see `meta.epoch`.
    pub epoch: String,
}

async fn health(State(st): State<AppState>) -> Json<HealthResp> {
    Json(HealthResp {
        status: "ok".into(),
        protocol: PROTOCOL_VERSION,
        epoch: st.epoch.clone(),
    })
}

/// Build the application router. Later tasks add their routes to
/// [`routes`].
pub fn router(state: AppState) -> Router {
    let limit = state.limits.max_body_bytes;
    routes()
        .fallback(|| async { ApiError::of(ErrorCode::NotFound) })
        .layer(RequestBodyLimitLayer::new(limit))
        .layer(middleware::from_fn(normalize_errors))
        .layer(middleware::from_fn(protocol_version))
        .layer(
            // Logs request id, method, matched route (never the raw path, which
            // may carry ids), status and latency. No headers, no bodies.
            TraceLayer::new_for_http()
                .make_span_with(|req: &Request<Body>| {
                    let route = req
                        .extensions()
                        .get::<MatchedPath>()
                        .map_or("-", MatchedPath::as_str);
                    tracing::info_span!(
                        "request",
                        id = %hex(&random_bytes::<8>()),
                        method = %req.method(),
                        route,
                        status = Empty,
                    )
                })
                .on_request(())
                .on_response(
                    |res: &Response, latency: std::time::Duration, span: &tracing::Span| {
                        span.record("status", res.status().as_u16());
                        tracing::info!(
                            status = res.status().as_u16(),
                            latency_ms = latency.as_millis() as u64,
                            "response"
                        );
                    },
                )
                .on_failure(()),
        )
        .with_state(state)
}

/// The route table (without layers or state).
fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/health", get(health))
        .merge(crate::account::routes())
        .merge(crate::accounts::routes())
        .merge(crate::auth::routes())
        .merge(crate::devices::routes())
        .merge(crate::sync::routes())
        .merge(crate::events::routes())
}

/// Reject clients from a different protocol major and stamp the server's own
/// on every response. A missing header is allowed (health checks, curl).
async fn protocol_version(req: Request, next: Next) -> Response {
    let verdict = match req.headers().get(HEADER_PROTOCOL) {
        None => Ok(()),
        Some(v) => match v
            .to_str()
            .ok()
            .and_then(|s| s.split('.').next())
            .and_then(|s| s.trim().parse::<u32>().ok())
        {
            None => Err(ApiError::bad_request("malformed X-Tether-Protocol header")),
            Some(m) if m > PROTOCOL_VERSION => Err(ApiError::of(ErrorCode::UpdateServer)),
            Some(m) if m < MIN_CLIENT_PROTOCOL => Err(ApiError::of(ErrorCode::UpdateClient)),
            Some(_) => Ok(()),
        },
    };
    let mut res = match verdict {
        Ok(()) => next.run(req).await,
        Err(e) => e.into_response(),
    };
    res.headers_mut().insert(
        HeaderName::from_static("x-tether-protocol"),
        HeaderValue::from(PROTOCOL_VERSION),
    );
    res
}

/// Turn any error response that is not already JSON (router 404/405, extractor
/// rejections, the body limit) into an [`ErrorBody`], so clients only ever
/// parse one error shape.
async fn normalize_errors(req: Request, next: Next) -> Response {
    let res = next.run(req).await;
    let status = res.status();
    if !(status.is_client_error() || status.is_server_error()) {
        return res;
    }
    let is_json = res
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"));
    if is_json {
        return res;
    }
    let code = code_for_status(status);
    let message = ApiError::of(code).message().to_owned();
    let mut out = (status, Json(ErrorBody { code, message })).into_response();
    // Keep headers such as `Allow` on a 405.
    for (k, v) in res.headers() {
        if k != header::CONTENT_TYPE && k != header::CONTENT_LENGTH {
            out.headers_mut().append(k.clone(), v.clone());
        }
    }
    debug_assert!(status != StatusCode::OK);
    out
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::config::{Config, TlsMode};
    use tower::ServiceExt;

    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Sink {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Sink {
        type Writer = Sink;
        fn make_writer(&'a self) -> Sink {
            self.clone()
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn logs_route_status_latency_but_no_headers_or_query() {
        let sink = Sink::default();
        let sub = tracing_subscriber::fmt()
            .with_writer(sink.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::DEBUG)
            .finish();
        let _guard = tracing::subscriber::set_default(sub);

        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config::new(dir.path().to_path_buf());
        cfg.tls = TlsMode::Off;
        let app = router(AppState::open(cfg, None).await.unwrap());

        let req = Request::builder()
            .uri("/v1/health?token=QUERY-SECRET")
            .header("authorization", "Bearer HEADER-SECRET")
            .body(Body::empty())
            .unwrap();
        assert_eq!(app.clone().oneshot(req).await.unwrap().status(), 200);
        let req = Request::builder()
            .uri("/v1/secret-path-SECRET")
            .body(Body::empty())
            .unwrap();
        assert_eq!(app.oneshot(req).await.unwrap().status(), 404);

        let log = String::from_utf8(sink.0.lock().unwrap().clone()).unwrap();
        assert!(log.contains("/v1/health"), "{log}");
        assert!(
            log.contains("status=200") || log.contains("status: 200"),
            "{log}"
        );
        assert!(log.contains("latency_ms"), "{log}");
        assert!(!log.contains("SECRET"), "{log}");
    }
}
