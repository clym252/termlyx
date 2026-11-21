//! The server side of a SOCKS5 handshake (RFC 1928), as used by dynamic
//! (`-D`) forwards.
//!
//! Only what a forward needs: the "no authentication" method and the
//! CONNECT command, to an IPv4, IPv6 or domain-name address. Domain names
//! are passed on unresolved (the SSH server resolves them, "socks5h"
//! semantics).
//!
//! Every read is exact and bounded (a domain name is at most 255 bytes), so
//! hostile input cannot make it allocate, loop or panic. A silent client is
//! the caller's to bound with a timeout.

use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const VERSION: u8 = 0x05;
const METHOD_NO_AUTH: u8 = 0x00;
const METHOD_NONE_ACCEPTABLE: u8 = 0xFF;
const CMD_CONNECT: u8 = 0x01;
const ATYP_IPV4: u8 = 0x01;
const ATYP_DOMAIN: u8 = 0x03;
const ATYP_IPV6: u8 = 0x04;

/// Reply codes (`REP`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Reply {
    Succeeded = 0x00,
    GeneralFailure = 0x01,
    NotAllowed = 0x02,
    HostUnreachable = 0x04,
    CommandNotSupported = 0x07,
    AddressTypeNotSupported = 0x08,
}

/// Where the client wants to connect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Target {
    Addr(SocketAddr),
    /// An unresolved host name and port.
    Domain(String, u16),
}

impl Target {
    /// Host (an IP literal or the name, unresolved) and port.
    pub(crate) fn host_port(&self) -> (String, u16) {
        match self {
            Target::Addr(a) => (a.ip().to_string(), a.port()),
            Target::Domain(name, port) => (name.clone(), *port),
        }
    }
}

/// Why a handshake ended without a target. Where the protocol has an
/// answer for it, the client has already been sent it.
#[derive(Debug, thiserror::Error)]
pub(crate) enum HandshakeError {
    #[error("i/o: {0}")]
    Io(#[from] io::Error),
    #[error("not SOCKS5 (version byte {0:#04x})")]
    BadVersion(u8),
    #[error("the client offers no acceptable authentication method")]
    NoAcceptableMethod,
    #[error("unsupported command {0:#04x}")]
    UnsupportedCommand(u8),
    #[error("unsupported address type {0:#04x}")]
    UnsupportedAddressType(u8),
    #[error("invalid domain name")]
    BadDomain,
}

/// Runs the server side of the handshake up to the client's request: method
/// negotiation (only "no authentication" is accepted), then one CONNECT
/// request. On success the caller must answer with [`reply`] once it knows
/// whether the target can be reached.
pub(crate) async fn handshake<S>(s: &mut S) -> Result<Target, HandshakeError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    // Greeting: VER NMETHODS METHODS...
    let [ver, nmethods] = read_array(s).await?;
    if ver != VERSION {
        return Err(HandshakeError::BadVersion(ver));
    }
    let mut methods = [0u8; 255];
    let methods = &mut methods[..usize::from(nmethods)];
    s.read_exact(methods).await?;
    if !methods.contains(&METHOD_NO_AUTH) {
        s.write_all(&[VERSION, METHOD_NONE_ACCEPTABLE]).await?;
        s.flush().await?;
        return Err(HandshakeError::NoAcceptableMethod);
    }
    s.write_all(&[VERSION, METHOD_NO_AUTH]).await?;
    s.flush().await?;

    // Request: VER CMD RSV ATYP DST.ADDR DST.PORT
    let [ver, cmd, _rsv, atyp] = read_array(s).await?;
    if ver != VERSION {
        reply_unbound(s, Reply::GeneralFailure).await?;
        return Err(HandshakeError::BadVersion(ver));
    }
    // Read the whole request (when its address type says how long it is)
    // before answering, so the reply is not followed by a reset for unread
    // bytes.
    let addr = match atyp {
        ATYP_IPV4 => {
            let ip = Ipv4Addr::from(read_array::<_, 4>(s).await?);
            Some(Ok(Target::Addr(SocketAddr::new(
                ip.into(),
                read_port(s).await?,
            ))))
        }
        ATYP_IPV6 => {
            let ip = Ipv6Addr::from(read_array::<_, 16>(s).await?);
            Some(Ok(Target::Addr(SocketAddr::new(
                ip.into(),
                read_port(s).await?,
            ))))
        }
        ATYP_DOMAIN => {
            let [len] = read_array(s).await?;
            let mut name = [0u8; 255];
            let name = &mut name[..usize::from(len)];
            s.read_exact(name).await?;
            let port = read_port(s).await?;
            Some(match std::str::from_utf8(name) {
                Ok(name) if !name.is_empty() => Ok(Target::Domain(name.to_owned(), port)),
                _ => Err(HandshakeError::BadDomain),
            })
        }
        // Unknown length: nothing more can be read.
        _ => None,
    };
    if cmd != CMD_CONNECT {
        reply_unbound(s, Reply::CommandNotSupported).await?;
        return Err(HandshakeError::UnsupportedCommand(cmd));
    }
    match addr {
        Some(Ok(target)) => Ok(target),
        Some(Err(e)) => {
            reply_unbound(s, Reply::GeneralFailure).await?;
            Err(e)
        }
        None => {
            reply_unbound(s, Reply::AddressTypeNotSupported).await?;
            Err(HandshakeError::UnsupportedAddressType(atyp))
        }
    }
}

/// Answers the client's request. `bound` is reported as the server's bound
/// address (`BND.ADDR`/`BND.PORT`).
pub(crate) async fn reply<S>(s: &mut S, code: Reply, bound: SocketAddr) -> io::Result<()>
where
    S: AsyncWrite + Unpin,
{
    s.write_all(&encode_reply(code, bound)).await?;
    s.flush().await
}

async fn reply_unbound<S>(s: &mut S, code: Reply) -> io::Result<()>
where
    S: AsyncWrite + Unpin,
{
    reply(s, code, SocketAddr::from(([0, 0, 0, 0], 0))).await
}

fn encode_reply(code: Reply, bound: SocketAddr) -> Vec<u8> {
    let mut out = vec![VERSION, code as u8, 0x00];
    match bound {
        SocketAddr::V4(a) => {
            out.push(ATYP_IPV4);
            out.extend_from_slice(&a.ip().octets());
        }
        SocketAddr::V6(a) => {
            out.push(ATYP_IPV6);
            out.extend_from_slice(&a.ip().octets());
        }
    }
    out.extend_from_slice(&bound.port().to_be_bytes());
    out
}

async fn read_array<S, const N: usize>(s: &mut S) -> io::Result<[u8; N]>
where
    S: AsyncRead + Unpin,
{
    let mut buf = [0u8; N];
    s.read_exact(&mut buf).await?;
    Ok(buf)
}

async fn read_port<S>(s: &mut S) -> io::Result<u16>
where
    S: AsyncRead + Unpin,
{
    Ok(u16::from_be_bytes(read_array(s).await?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{DuplexStream, duplex};

    /// Runs the handshake against `client_bytes` (then EOF from the client);
    /// returns its result, everything the server wrote, and the client
    /// bytes it left unread.
    async fn run(client_bytes: &[u8]) -> (Result<Target, HandshakeError>, Vec<u8>, Vec<u8>) {
        let (mut client, mut server): (DuplexStream, DuplexStream) = duplex(4096);
        client.write_all(client_bytes).await.unwrap();
        client.shutdown().await.unwrap();
        let result = handshake(&mut server).await;
        // Whatever the client sent that the handshake did not read.
        let mut unread = Vec::new();
        server.read_to_end(&mut unread).await.unwrap();
        drop(server);
        let mut written = Vec::new();
        client.read_to_end(&mut written).await.unwrap();
        (result, written, unread)
    }

    const NO_AUTH_OK: [u8; 2] = [0x05, 0x00];

    fn reply_bytes(code: u8) -> Vec<u8> {
        vec![0x05, code, 0x00, 0x01, 0, 0, 0, 0, 0, 0]
    }

    #[tokio::test]
    async fn connect_ipv4() {
        let (r, out, unread) = run(&[5, 1, 0, 5, 1, 0, 1, 10, 0, 0, 7, 0x1f, 0x90]).await;
        assert_eq!(r.unwrap(), Target::Addr("10.0.0.7:8080".parse().unwrap()));
        assert_eq!(out, NO_AUTH_OK);
        assert!(unread.is_empty(), "left unread: {unread:?}");
    }

    #[tokio::test]
    async fn connect_ipv6() {
        let mut req = vec![5, 2, 2, 0, 5, 1, 0, 4];
        req.extend_from_slice(&"2001:db8::1".parse::<Ipv6Addr>().unwrap().octets());
        req.extend_from_slice(&443u16.to_be_bytes());
        let (r, out, unread) = run(&req).await;
        assert_eq!(
            r.unwrap(),
            Target::Addr("[2001:db8::1]:443".parse().unwrap())
        );
        assert_eq!(out, NO_AUTH_OK, "no-auth picked among several methods");
        assert!(unread.is_empty(), "left unread: {unread:?}");
    }

    #[tokio::test]
    async fn connect_domain_is_not_resolved() {
        let mut req = vec![5, 1, 0, 5, 1, 0, 3, 3];
        req.extend_from_slice(b"web");
        req.extend_from_slice(&80u16.to_be_bytes());
        let (r, out, unread) = run(&req).await;
        let target = r.unwrap();
        assert_eq!(target, Target::Domain("web".into(), 80));
        assert_eq!(target.host_port(), ("web".to_owned(), 80));
        assert_eq!(out, NO_AUTH_OK);
        assert!(unread.is_empty(), "left unread: {unread:?}");
    }

    #[tokio::test]
    async fn bad_version_is_rejected_without_reply() {
        // A SOCKS4 CONNECT.
        let (r, out, unread) = run(&[4, 1, 0, 80, 127, 0, 0, 1, 0]).await;
        assert!(matches!(r, Err(HandshakeError::BadVersion(4))), "{r:?}");
        assert!(out.is_empty());
        assert_eq!(unread.len(), 7, "the rest is not read");
    }

    #[tokio::test]
    async fn bad_request_version_gets_general_failure() {
        let (r, out, unread) = run(&[5, 1, 0, 4, 1, 0, 1, 1, 2, 3, 4, 0, 80]).await;
        assert!(matches!(r, Err(HandshakeError::BadVersion(4))), "{r:?}");
        assert_eq!(out, [&NO_AUTH_OK[..], &reply_bytes(0x01)].concat());
        assert_eq!(unread.len(), 6, "framing unknown: the address is not read");
    }

    /// The whole request is read before the 0x07 reply (so closing right
    /// after it does not reset the connection over unread bytes), for every
    /// address type.
    #[tokio::test]
    async fn non_connect_commands_get_0x07() {
        let mut v6 = vec![4];
        v6.extend_from_slice(&[0u8; 15]);
        v6.push(1);
        let addresses: [&[u8]; 3] = [&[1, 1, 2, 3, 4], &v6, &[3, 3, b'w', b'e', b'b']];
        for cmd in [0x02u8 /* BIND */, 0x03 /* UDP ASSOCIATE */, 0x09] {
            for addr in addresses {
                let mut req = vec![5, 1, 0, 5, cmd, 0];
                req.extend_from_slice(addr);
                req.extend_from_slice(&80u16.to_be_bytes());
                let (r, out, unread) = run(&req).await;
                assert!(
                    matches!(r, Err(HandshakeError::UnsupportedCommand(c)) if c == cmd),
                    "{r:?}"
                );
                assert_eq!(out, [&NO_AUTH_OK[..], &reply_bytes(0x07)].concat());
                assert!(unread.is_empty(), "left unread: {unread:?}");
            }
        }
        // An unknown address type with an unknown command: the command is
        // reported.
        let (r, out, _) = run(&[5, 1, 0, 5, 2, 0, 9, 1, 2]).await;
        assert!(
            matches!(r, Err(HandshakeError::UnsupportedCommand(2))),
            "{r:?}"
        );
        assert_eq!(out, [&NO_AUTH_OK[..], &reply_bytes(0x07)].concat());
    }

    #[tokio::test]
    async fn no_acceptable_auth_gets_0xff() {
        // Only username/password (0x02) and GSSAPI (0x01) offered.
        let (r, out, _) = run(&[5, 2, 2, 1]).await;
        assert!(
            matches!(r, Err(HandshakeError::NoAcceptableMethod)),
            "{r:?}"
        );
        assert_eq!(out, [0x05, 0xFF]);
        // No methods at all.
        let (r, out, _) = run(&[5, 0]).await;
        assert!(
            matches!(r, Err(HandshakeError::NoAcceptableMethod)),
            "{r:?}"
        );
        assert_eq!(out, [0x05, 0xFF]);
    }

    #[tokio::test]
    async fn unknown_address_type_gets_0x08() {
        let (r, out, unread) = run(&[5, 1, 0, 5, 1, 0, 9, 1, 2, 3, 4]).await;
        assert!(
            matches!(r, Err(HandshakeError::UnsupportedAddressType(9))),
            "{r:?}"
        );
        assert_eq!(out, [&NO_AUTH_OK[..], &reply_bytes(0x08)].concat());
        assert_eq!(unread, [1, 2, 3, 4], "length unknown: nothing more is read");
    }

    #[tokio::test]
    async fn empty_or_non_utf8_domain_gets_general_failure() {
        for name in [&b""[..], &[0xff, 0xfe][..]] {
            let mut req = vec![5, 1, 0, 5, 1, 0, 3, name.len() as u8];
            req.extend_from_slice(name);
            req.extend_from_slice(&80u16.to_be_bytes());
            let (r, out, unread) = run(&req).await;
            assert!(matches!(r, Err(HandshakeError::BadDomain)), "{r:?}");
            assert_eq!(out, [&NO_AUTH_OK[..], &reply_bytes(0x01)].concat());
            assert!(unread.is_empty(), "left unread: {unread:?}");
        }
    }

    #[tokio::test]
    async fn truncated_input_is_an_error_at_every_cut() {
        let mut full = vec![5, 1, 0, 5, 1, 0, 3, 3];
        full.extend_from_slice(b"web");
        full.extend_from_slice(&80u16.to_be_bytes());
        for cut in 0..full.len() {
            let (r, _, _) = run(&full[..cut]).await;
            assert!(matches!(r, Err(HandshakeError::Io(_))), "cut {cut}: {r:?}");
        }
    }

    #[test]
    fn replies_encode_the_bound_address() {
        assert_eq!(
            encode_reply(Reply::Succeeded, "127.0.0.1:1080".parse().unwrap()),
            [5, 0, 0, 1, 127, 0, 0, 1, 0x04, 0x38]
        );
        let v6 = encode_reply(Reply::HostUnreachable, "[::1]:443".parse().unwrap());
        assert_eq!(&v6[..4], [5, 4, 0, 4]);
        assert_eq!(v6.len(), 4 + 16 + 2);
        assert_eq!(&v6[20..], 443u16.to_be_bytes());
    }
}
