//! The events WebSocket (`GET /v1/events`, `docs/protocol.md`).
//!
//! The access token travels in the `Sec-WebSocket-Protocol` header as
//! `bearer.<token>` next to `tether.v1`, never in the URL. The server must
//! select `tether.v1`. The server pings every 30 s; tungstenite answers pings
//! while the stream is being polled.
//!
//! # Logging hazard
//!
//! The access token is in the upgrade request, and tungstenite writes that
//! whole request to the log at `trace` (`tungstenite::handshake::client`).
//! Embedding binaries must therefore apply [`crate::LOG_DIRECTIVES`] after
//! the user's log filter so `RUST_LOG=trace` cannot expose live tokens
//! (`tests/client.rs: tokens_never_reach_logs` proves both halves).

use std::time::Duration;

use core_proto::{
    ErrorBody, Event, HEADER_DEVICE, HEADER_PROTOCOL, HEADER_SCHEMA, KNOWN_SCHEMA,
    PROTOCOL_VERSION, WS_PROTOCOL, ws_bearer_protocol,
};
use futures_util::{Stream, StreamExt};
use reqwest::header::HeaderValue;
use secrecy::{ExposeSecret, SecretString};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{
    Connector, MaybeTlsStream, WebSocketStream, connect_async_tls_with_config,
};

use crate::client::{Client, newer_protocol};
use crate::error::SyncError;
use crate::tls;

/// Time allowed for the TCP, TLS and upgrade handshakes together.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Events are tiny; a larger frame is a misbehaving peer.
const MAX_FRAME: usize = 64 * 1024;

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

enum State {
    Start(Client),
    Open(Box<Socket>),
    Done,
}

/// Connect to the server's event stream and yield its [`Event`]s.
///
/// The connection is made when the stream is first polled. A failure to
/// connect is the stream's only item (`Err`), after which it ends; so does a
/// transport error later. The stream also ends, without an error, when the
/// server closes the socket (it does so right after [`Event::Revoked`]); the
/// engine reconnects with backoff and then pulls `changes`. The access token
/// is refreshed first if it is close to expiry, and once more if the upgrade
/// is refused as `unauthorized`. A certificate that does not match the pin is
/// [`SyncError::CertificateChanged`].
pub fn connect(client: &Client) -> impl Stream<Item = Result<Event, SyncError>> + Send + 'static {
    futures_util::stream::unfold(State::Start(client.clone()), step)
}

/// Like [`connect`], but performs the handshake first: `Ok` means the socket
/// is open (so a caller can pull right away and miss nothing), and the stream
/// then yields its events as [`connect`]'s does.
pub async fn open_stream(
    client: &Client,
) -> Result<impl Stream<Item = Result<Event, SyncError>> + Send + 'static, SyncError> {
    let socket = open(client).await?;
    Ok(futures_util::stream::unfold(
        State::Open(Box::new(socket)),
        step,
    ))
}

async fn step(state: State) -> Option<(Result<Event, SyncError>, State)> {
    let mut socket = match state {
        State::Done => return None,
        State::Start(client) => match open(&client).await {
            Ok(socket) => Box::new(socket),
            Err(e) => return Some((Err(e), State::Done)),
        },
        State::Open(socket) => socket,
    };
    loop {
        match socket.next().await {
            Some(Ok(Message::Text(text))) => match serde_json::from_str::<Event>(&text) {
                Ok(event) => return Some((Ok(event), State::Open(socket))),
                // An event type from a newer server: ignore it. Pulling
                // `changes` after any event makes missing one harmless.
                Err(_) => tracing::debug!(len = text.len(), "ignoring unknown event frame"),
            },
            // Ping, pong, binary: tungstenite already answered pings.
            Some(Ok(
                Message::Ping(_) | Message::Pong(_) | Message::Binary(_) | Message::Frame(_),
            )) => {}
            Some(Ok(Message::Close(_))) | None => return None,
            Some(Err(WsError::ConnectionClosed | WsError::AlreadyClosed)) => return None,
            Some(Err(e)) => return Some((Err(tls::classify(&e, false)), State::Done)),
        }
    }
}

/// Open the socket, refreshing the access token first when needed and once
/// more if the upgrade is refused as `unauthorized`.
async fn open(client: &Client) -> Result<Socket, SyncError> {
    let (token, seen) = client.access().await?;
    match handshake(client, &token).await {
        Err(SyncError::Unauthorized) if client.has_session() => {
            client.refresh_after(seen).await?;
            let (token, _) = client.access().await?;
            handshake(client, &token).await
        }
        other => other,
    }
}

async fn handshake(client: &Client, token: &SecretString) -> Result<Socket, SyncError> {
    let protocols = format!(
        "{WS_PROTOCOL}, {}",
        ws_bearer_protocol(token.expose_secret())
    );
    let mut request = client
        .events_url()
        .into_client_request()
        .map_err(|_| SyncError::Protocol("server address is not valid".into()))?;
    let headers = request.headers_mut();
    let mut offered = HeaderValue::from_str(&protocols)
        .map_err(|_| SyncError::Protocol("a credential has characters HTTP cannot carry".into()))?;
    offered.set_sensitive(true);
    headers.insert("sec-websocket-protocol", offered);
    headers.insert(
        HEADER_PROTOCOL,
        HeaderValue::from_str(&PROTOCOL_VERSION.to_string()).expect("digits"),
    );
    headers.insert(
        HEADER_SCHEMA,
        HeaderValue::from_str(&KNOWN_SCHEMA.to_string()).expect("digits"),
    );
    if let Some(device) = client.device() {
        headers.insert(
            HEADER_DEVICE,
            HeaderValue::from_str(&device.to_string()).expect("uuid"),
        );
    }

    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_FRAME))
        .max_frame_size(Some(MAX_FRAME));
    let connector = Connector::Rustls(client.tls_config());
    let attempt = connect_async_tls_with_config(request, Some(config), false, Some(connector));
    let (socket, response) = tokio::time::timeout(HANDSHAKE_TIMEOUT, attempt)
        .await
        .map_err(|_| SyncError::Unreachable("timed out".into()))?
        .map_err(refused)?;

    if newer_protocol(response.headers()) {
        return Err(SyncError::UpdateClient);
    }
    let selected = response
        .headers()
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok());
    if selected != Some(WS_PROTOCOL) {
        return Err(SyncError::Protocol(
            "the server did not select the sync protocol".into(),
        ));
    }
    Ok(socket)
}

/// A failed handshake: an ordinary error body when the server refused the
/// upgrade, otherwise a transport failure.
fn refused(e: WsError) -> SyncError {
    match e {
        WsError::Http(response) => {
            if newer_protocol(response.headers()) {
                return SyncError::UpdateClient;
            }
            let body = response.body().as_deref().unwrap_or_default();
            match serde_json::from_slice::<ErrorBody>(body) {
                Ok(err) => SyncError::from_body(err),
                Err(_) => SyncError::Protocol(format!(
                    "unexpected HTTP status {}",
                    response.status().as_u16()
                )),
            }
        }
        WsError::Url(_) | WsError::HttpFormat(_) => {
            SyncError::Protocol("server address is not valid".into())
        }
        WsError::Protocol(_) => {
            SyncError::Protocol("the server did not accept the sync protocol".into())
        }
        other => tls::classify(&other, false),
    }
}
