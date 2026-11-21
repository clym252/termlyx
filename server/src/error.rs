//! The server's error type and its mapping to HTTP.
//!
//! Every non-2xx response carries a [`core_proto::ErrorBody`]. The message is
//! plain language that is safe to show a user; it never echoes request bodies,
//! tokens or key material, and internal failures are logged here and answered
//! with a generic message.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use core_proto::{ErrorBody, ErrorCode};

/// An error that becomes an HTTP response with an [`ErrorBody`].
#[derive(Debug, thiserror::Error)]
#[error("{code:?}: {message}")]
pub struct ApiError {
    code: ErrorCode,
    message: String,
}

/// The HTTP status each [`ErrorCode`] is answered with (also documented in
/// `docs/protocol.md`).
pub fn status_for(code: ErrorCode) -> StatusCode {
    match code {
        ErrorCode::BadRequest => StatusCode::BAD_REQUEST,
        ErrorCode::Unauthorized | ErrorCode::Revoked => StatusCode::UNAUTHORIZED,
        ErrorCode::Forbidden => StatusCode::FORBIDDEN,
        ErrorCode::NotFound => StatusCode::NOT_FOUND,
        ErrorCode::Conflict | ErrorCode::UsernameTaken => StatusCode::CONFLICT,
        ErrorCode::Gone => StatusCode::GONE,
        ErrorCode::ClockSkew | ErrorCode::UpdateClient | ErrorCode::UpdateServer => {
            StatusCode::BAD_REQUEST
        }
        ErrorCode::InviteInvalid => StatusCode::FORBIDDEN,
        ErrorCode::RateLimited => StatusCode::TOO_MANY_REQUESTS,
        ErrorCode::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
        ErrorCode::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// The error class for an HTTP status produced by a library (extractor
/// rejection, router 404/405, body limit) rather than by a handler.
pub fn code_for_status(status: StatusCode) -> ErrorCode {
    match status.as_u16() {
        401 => ErrorCode::Unauthorized,
        403 => ErrorCode::Forbidden,
        404 => ErrorCode::NotFound,
        409 => ErrorCode::Conflict,
        410 => ErrorCode::Gone,
        413 => ErrorCode::PayloadTooLarge,
        429 => ErrorCode::RateLimited,
        s if (400..500).contains(&s) => ErrorCode::BadRequest,
        _ => ErrorCode::Internal,
    }
}

/// Default plain-language message for a code.
fn default_message(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::BadRequest => "the request was not understood",
        ErrorCode::Unauthorized => "sign in again",
        ErrorCode::Revoked => "device revoked",
        ErrorCode::Forbidden => "not allowed",
        ErrorCode::NotFound => "not found",
        ErrorCode::Conflict => "the data changed; refresh and try again",
        ErrorCode::Gone => "no longer available",
        ErrorCode::ClockSkew => "this device's clock is wrong",
        ErrorCode::RateLimited => "too many attempts; try again later",
        ErrorCode::UpdateClient => "update Tether",
        ErrorCode::UpdateServer => "the Tether server is older than this app; update the server",
        ErrorCode::InviteInvalid => "this invite is not valid",
        ErrorCode::UsernameTaken => "that username is taken",
        ErrorCode::PayloadTooLarge => "the request is too large",
        ErrorCode::Internal => "internal server error",
    }
}

impl ApiError {
    /// An error with an explicit message.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// An error with the code's default message.
    pub fn of(code: ErrorCode) -> Self {
        Self::new(code, default_message(code))
    }

    /// 400 `bad_request`.
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::BadRequest, message)
    }

    /// 500 `internal`, with a generic message. The cause is logged, never sent.
    pub fn internal(cause: impl std::fmt::Display) -> Self {
        tracing::error!(error = %cause, "internal error");
        Self::of(ErrorCode::Internal)
    }

    /// The machine-readable class.
    pub fn code(&self) -> ErrorCode {
        self.code
    }

    /// The user-safe message.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// The wire body.
    pub fn body(&self) -> ErrorBody {
        ErrorBody {
            code: self.code,
            message: self.message.clone(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (status_for(self.code), Json(self.body())).into_response()
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        Self::internal(e)
    }
}

/// Server result alias for handlers.
pub type ApiResult<T> = Result<T, ApiError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_code_has_a_status_and_message() {
        use ErrorCode::*;
        for c in [
            BadRequest,
            Unauthorized,
            Revoked,
            Forbidden,
            NotFound,
            Conflict,
            Gone,
            ClockSkew,
            RateLimited,
            UpdateClient,
            UpdateServer,
            InviteInvalid,
            UsernameTaken,
            PayloadTooLarge,
            Internal,
        ] {
            assert!(status_for(c).as_u16() >= 400);
            assert!(!default_message(c).is_empty());
        }
        assert_eq!(status_for(UpdateServer), StatusCode::BAD_REQUEST);
        assert_eq!(status_for(Revoked), StatusCode::UNAUTHORIZED);
        assert_eq!(default_message(Revoked), "device revoked");
        assert_eq!(default_message(UpdateClient), "update Tether");
    }

    #[test]
    fn internal_errors_do_not_leak_the_cause() {
        let e = ApiError::internal("secret path /data/x failed");
        assert_eq!(e.code(), ErrorCode::Internal);
        assert!(!e.message().contains("secret"));
    }
}
