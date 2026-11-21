//! `client::Config::manual_window_adjust`: the receive window is only
//! refilled by `WindowGranter::adjust_window` (`Handle::window_granter`),
//! per channel.

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;

use russh::keys::{PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use russh::server::{self, Auth, Msg, Server as _, Session};
use russh::{Channel, ChannelMsg, client};
use ssh_key::PrivateKey;
use tokio::time::{Duration, sleep, timeout};

const WINDOW: u32 = 64 * 1024;
const CHUNK: usize = 8 * 1024;
/// Each channel's server task sends this much (4 windows).
const TOTAL: usize = 4 * WINDOW as usize;

#[tokio::test]
async fn manual_window_only_refills_on_adjust() -> Result<(), anyhow::Error> {
    let addr = addr();
    tokio::spawn(Sender::run(addr));
    while TcpStream::connect(addr).is_err() {
        sleep(Duration::from_millis(10)).await;
    }

    let config = Arc::new(client::Config {
        window_size: WINDOW,
        manual_window_adjust: true,
        ..Default::default()
    });
    let key = Arc::new(PrivateKey::random(&mut rand::rng(), ssh_key::Algorithm::Ed25519).unwrap());
    let mut session = client::connect(config, addr, Client).await?;
    let auth = session
        .authenticate_publickey(
            "user",
            PrivateKeyWithHashAlg::new(key, session.best_supported_rsa_hash().await?.flatten()),
        )
        .await?;
    assert!(auth.success());

    // Two channels; `stalled` is drained but never granted more window.
    let mut stalled = session.channel_open_session().await?;
    let mut flowing = session.channel_open_session().await?;
    let granter = session.window_granter();

    // Without adjusting, each channel gets exactly one window.
    assert_eq!(drain(&mut stalled, WINDOW as usize).await, WINDOW as usize);
    assert_eq!(drain(&mut flowing, WINDOW as usize).await, WINDOW as usize);
    assert!(
        timeout(Duration::from_millis(300), stalled.wait()).await.is_err(),
        "the server must be out of window"
    );

    // Granting window on `flowing` moves only `flowing`, all the way.
    let mut got = WINDOW as usize;
    while got < TOTAL {
        granter.adjust_window(flowing.id(), WINDOW)?;
        got += drain(&mut flowing, WINDOW as usize).await;
    }
    assert_eq!(got, TOTAL);
    assert!(timeout(Duration::from_millis(300), stalled.wait()).await.is_err());

    // And `stalled` resumes once it is granted window.
    granter.adjust_window(stalled.id(), WINDOW)?;
    assert_eq!(drain(&mut stalled, WINDOW as usize).await, WINDOW as usize);
    Ok(())
}

/// Receives exactly `want` data bytes (within 5 s).
async fn drain(ch: &mut Channel<client::Msg>, want: usize) -> usize {
    let fut = async {
        let mut got = 0;
        while got < want {
            match ch.wait().await {
                Some(ChannelMsg::Data { data }) => got += data.len(),
                Some(ChannelMsg::WindowAdjusted { .. }) => {}
                other => panic!("unexpected {other:?}"),
            }
        }
        got
    };
    timeout(Duration::from_secs(5), fut)
        .await
        .expect("data did not arrive")
}

fn addr() -> SocketAddr {
    TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
}

struct Client;

impl client::Handler for Client {
    type Error = anyhow::Error;

    async fn check_server_key(&mut self, _: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

/// Sends `TOTAL` bytes on every session channel, as fast as the window allows.
#[derive(Clone)]
struct Sender;

impl Sender {
    async fn run(addr: SocketAddr) {
        let config = Arc::new(server::Config {
            keys: vec![PrivateKey::random(&mut rand::rng(), ssh_key::Algorithm::Ed25519).unwrap()],
            ..Default::default()
        });
        Sender.run_on_address(config, addr).await.unwrap();
    }
}

impl server::Server for Sender {
    type Handler = Self;

    fn new_client(&mut self, _: Option<SocketAddr>) -> Self::Handler {
        self.clone()
    }
}

impl server::Handler for Sender {
    type Error = anyhow::Error;

    async fn auth_publickey(&mut self, _: &str, _: &ssh_key::PublicKey) -> Result<Auth, Self::Error> {
        Ok(Auth::Accept)
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        tokio::spawn(async move {
            for _ in 0..TOTAL / CHUNK {
                if channel.data_bytes(vec![7u8; CHUNK]).await.is_err() {
                    return;
                }
            }
        });
        Ok(())
    }
}
