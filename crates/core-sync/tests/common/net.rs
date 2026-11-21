//! Network stand-ins: a scriptable plain-HTTP stub server (for answers the
//! real server never gives) and a TCP proxy that can take a device offline.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

/// One request the stub received.
#[derive(Debug, Clone)]
pub struct Req {
    pub method: String,
    /// Path including the query string.
    pub path: String,
    pub body: Vec<u8>,
}

impl Req {
    /// `"METHOD /path"` without the query string.
    pub fn route(&self) -> String {
        let path = self.path.split('?').next().unwrap_or_default();
        format!("{} {path}", self.method)
    }
}

/// A stub answer.
pub struct Reply {
    pub status: u16,
    pub body: String,
}

pub fn json(status: u16, body: impl Into<String>) -> Reply {
    Reply {
        status,
        body: body.into(),
    }
}

pub fn error(status: u16, code: &str) -> Reply {
    json(
        status,
        format!(r#"{{"code":"{code}","message":"stub refused"}}"#),
    )
}

/// A plain-HTTP loopback server; every request (one per connection) goes to
/// the handler and is logged.
pub struct Stub {
    pub url: String,
    pub log: Arc<Mutex<Vec<Req>>>,
}

impl Stub {
    /// How many requests matched `route` (`"METHOD /path"`).
    pub fn count(&self, route: &str) -> usize {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.route() == route)
            .count()
    }
}

pub async fn stub(handler: impl Fn(&Req) -> Reply + Send + Sync + 'static) -> Stub {
    stub_with(handler, None).await
}

/// [`stub`] over TLS with a fresh self-signed certificate for `localhost`,
/// at `https://127.0.0.1:<port>`. Returns its pin too.
pub async fn stub_tls(
    handler: impl Fn(&Req) -> Reply + Send + Sync + 'static,
) -> (Stub, core_sync::Pin) {
    let key = rcgen::KeyPair::generate().unwrap();
    let cert = rcgen::CertificateParams::new(vec!["localhost".to_owned()])
        .unwrap()
        .self_signed(&key)
        .unwrap();
    let pin = core_sync::Pin::of_certificate(cert.der()).unwrap();
    let der = rustls::pki_types::PrivateKeyDer::Pkcs8(key.serialize_der().into());
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(vec![cert.der().clone()], der)
    .unwrap();
    (stub_with(handler, Some(Arc::new(config))).await, pin)
}

async fn stub_with(
    handler: impl Fn(&Req) -> Reply + Send + Sync + 'static,
    tls: Option<Arc<rustls::ServerConfig>>,
) -> Stub {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let scheme = if tls.is_some() { "https" } else { "http" };
    let url = format!("{scheme}://{}", listener.local_addr().unwrap());
    let log = Arc::new(Mutex::new(Vec::new()));
    let handler = Arc::new(handler);
    let seen = log.clone();
    let acceptor = tls.map(tokio_rustls::TlsAcceptor::from);
    tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            let (handler, seen, acceptor) = (handler.clone(), seen.clone(), acceptor.clone());
            tokio::spawn(async move {
                let _ = match acceptor {
                    None => serve_one(sock, &*handler, &seen).await,
                    Some(a) => match a.accept(sock).await {
                        Ok(tls) => serve_one(tls, &*handler, &seen).await,
                        Err(e) => Err(e),
                    },
                };
            });
        }
    });
    Stub { url, log }
}

async fn serve_one(
    mut sock: impl AsyncRead + AsyncWrite + Unpin,
    handler: &(dyn Fn(&Req) -> Reply + Send + Sync),
    log: &Mutex<Vec<Req>>,
) -> std::io::Result<()> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let head_end = loop {
        let n = sock.read(&mut chunk).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.lines();
    let mut first = lines.next().unwrap_or_default().split(' ');
    let method = first.next().unwrap_or_default().to_owned();
    let path = first.next().unwrap_or_default().to_owned();
    let length = lines
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    while buf.len() < head_end + length {
        let n = sock.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let req = Req {
        method,
        path,
        body: buf[head_end..].to_vec(),
    };
    log.lock().unwrap().push(req.clone());
    let reply = handler(&req);
    let out = format!(
        "HTTP/1.1 {} Stub\r\nContent-Type: application/json\r\nX-Tether-Protocol: 1\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{}",
        reply.status,
        reply.body.len(),
        reply.body
    );
    sock.write_all(out.as_bytes()).await?;
    sock.shutdown().await
}

/// A TCP forwarder in front of a server. While "down" it drops every
/// connection, including the ones already open. Its address stays the same
/// when the server behind it moves ([`Proxy::retarget`], e.g. after a
/// restart on a new port).
pub struct Proxy {
    pub url: String,
    down: Arc<AtomicBool>,
    conns: Arc<Mutex<Vec<JoinHandle<()>>>>,
    target: Arc<Mutex<SocketAddr>>,
}

impl Proxy {
    pub fn set_down(&self, down: bool) {
        self.down.store(down, Ordering::SeqCst);
        if down {
            for c in self.conns.lock().unwrap().drain(..) {
                c.abort();
            }
        }
    }

    /// New connections go to `to` from now on.
    pub fn retarget(&self, to: SocketAddr) {
        *self.target.lock().unwrap() = to;
    }
}

pub async fn proxy(target: SocketAddr) -> Proxy {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("https://{}", listener.local_addr().unwrap());
    let down = Arc::new(AtomicBool::new(false));
    let conns: Arc<Mutex<Vec<JoinHandle<()>>>> = Arc::new(Mutex::new(Vec::new()));
    let target = Arc::new(Mutex::new(target));
    let (d, c, t) = (down.clone(), conns.clone(), target.clone());
    tokio::spawn(async move {
        while let Ok((mut inbound, _)) = listener.accept().await {
            if d.load(Ordering::SeqCst) {
                drop(inbound);
                continue;
            }
            let target = *t.lock().unwrap();
            let task = tokio::spawn(async move {
                if let Ok(mut outbound) = TcpStream::connect(target).await {
                    let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                }
            });
            c.lock().unwrap().push(task);
        }
    });
    Proxy {
        url,
        down,
        conns,
        target,
    }
}
