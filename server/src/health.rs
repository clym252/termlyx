//! `tether-server health`: the container healthcheck.
//!
//! Connects to this server's own listener on loopback, trusts exactly the
//! certificate it serves (the pin of `{data}/tls/cert.pem`, or of the operator's
//! `files` certificate; never WebPKI, so it works offline and with a
//! self-signed certificate whose name is not `127.0.0.1`), asks
//! `GET /v1/health` and succeeds only on `{"status":"ok"}`. With TLS `off` it
//! speaks plain HTTP. It is blocking and small on purpose: no HTTP client crate.

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::time::Duration;

use anyhow::{Context, bail};
use rustls::pki_types::ServerName;
use rustls::{ClientConnection, StreamOwned};

use crate::config::Config;
use crate::http::HealthResp;
use crate::tls;

/// Connect, handshake and read must each finish within this.
const TIMEOUT: Duration = Duration::from_secs(5);
/// A health answer is a few dozen bytes; refuse anything absurd.
const MAX_RESPONSE: u64 = 64 * 1024;

/// Where to reach the listener from inside the same host: its configured port,
/// on loopback when it listens on a wildcard address.
pub fn local_addr(cfg: &Config) -> SocketAddr {
    let ip = match cfg.bind.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        ip => ip,
    };
    SocketAddr::new(ip, cfg.bind.port())
}

/// Probe the local server; `Ok` only when it answers `200` with
/// `{"status":"ok"}`.
pub async fn check(cfg: &Config) -> anyhow::Result<HealthResp> {
    let addr = local_addr(cfg);
    let pin = tls::local_pin(cfg).context("cannot read the local certificate")?;
    tokio::task::spawn_blocking(move || probe(addr, pin))
        .await
        .context("health probe panicked")?
}

fn probe(addr: SocketAddr, pin: Option<[u8; 32]>) -> anyhow::Result<HealthResp> {
    let tcp =
        TcpStream::connect_timeout(&addr, TIMEOUT).with_context(|| format!("connect {addr}"))?;
    tcp.set_read_timeout(Some(TIMEOUT))?;
    tcp.set_write_timeout(Some(TIMEOUT))?;
    let request = format!(
        "GET /v1/health HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nAccept: application/json\r\n\r\n"
    );
    let mut raw = Vec::new();
    match pin {
        None => {
            let mut tcp = tcp;
            tcp.write_all(request.as_bytes())?;
            // A reset after the full answer is still an answer.
            let _ = tcp.take(MAX_RESPONSE).read_to_end(&mut raw);
        }
        Some(pin) => {
            let name = ServerName::try_from("localhost").expect("valid name");
            let conn = ClientConnection::new(tls::pinned_client_config(pin), name)
                .context("tls client")?;
            let mut stream = StreamOwned::new(conn, tcp);
            stream
                .write_all(request.as_bytes())
                .context("tls handshake (does the certificate match?)")?;
            // Servers may close without close_notify; keep what was read.
            let _ = (&mut stream).take(MAX_RESPONSE).read_to_end(&mut raw);
        }
    }
    parse(&raw)
}

/// Check the status line and body of a raw HTTP/1.1 response.
fn parse(raw: &[u8]) -> anyhow::Result<HealthResp> {
    let text = String::from_utf8_lossy(raw);
    let Some((head, body)) = text.split_once("\r\n\r\n") else {
        bail!("no complete HTTP response ({} bytes)", raw.len());
    };
    let status = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or("");
    if status != "200" {
        bail!("health answered HTTP {status}");
    }
    // The body is a JSON object; tolerate chunk-size lines around it.
    let (Some(start), Some(end)) = (body.find('{'), body.rfind('}')) else {
        bail!("health answered without a JSON body");
    };
    let resp: HealthResp = serde_json::from_str(&body[start..=end]).context("health body")?;
    if resp.status != "ok" {
        bail!("health status is {:?}", resp.status);
    }
    Ok(resp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ok_and_rejects_the_rest() {
        let ok = "HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n{\"status\":\"ok\",\"protocol\":1,\"epoch\":\"e\"}";
        assert_eq!(parse(ok.as_bytes()).unwrap().protocol, 1);
        let chunked = "HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n2a\r\n{\"status\":\"ok\",\"protocol\":1,\"epoch\":\"e\"}\r\n0\r\n\r\n";
        assert!(parse(chunked.as_bytes()).is_ok());
        for bad in [
            "",
            "HTTP/1.1 200 OK",
            "HTTP/1.1 503 Service Unavailable\r\n\r\n{\"status\":\"ok\",\"protocol\":1,\"epoch\":\"e\"}",
            "HTTP/1.1 200 OK\r\n\r\n{\"status\":\"down\",\"protocol\":1,\"epoch\":\"e\"}",
            "HTTP/1.1 200 OK\r\n\r\nnot json",
        ] {
            assert!(parse(bad.as_bytes()).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn wildcard_binds_probe_loopback() {
        let mut cfg = Config::new("/nonexistent".into());
        assert_eq!(local_addr(&cfg), "127.0.0.1:8443".parse().unwrap());
        cfg.bind = "[::]:9000".parse().unwrap();
        assert_eq!(local_addr(&cfg), "[::1]:9000".parse().unwrap());
        cfg.bind = "192.168.1.8:8443".parse().unwrap();
        assert_eq!(local_addr(&cfg), "192.168.1.8:8443".parse().unwrap());
    }
}
