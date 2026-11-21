//! `tether-server` binary: all logic lives in the library.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tether_server::cli::run().await
}
