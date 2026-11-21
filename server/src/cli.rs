//! Command line: `serve` (default), `fingerprint`, `health`, `invite`, `link`, the
//! `users` and `devices` admin subcommands, and `backup` / `restore`. Every
//! subcommand reads the same `TERMLYX_*` configuration as `serve`, so
//! `docker compose exec tether termlyx-server <subcommand>` works.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use core_proto::{CodeKind, JoinCode};

use crate::config::{Config, TlsMode};
use crate::util::system_now_ms;
use crate::{accounts, admin, backup, db, tls};

/// Termlyx sync server.
#[derive(Debug, Parser)]
#[command(name = "termlyx-server", version, about)]
pub struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the server (the default).
    Serve,
    /// Print the TLS certificate's public-key pin and names, creating the
    /// self-signed certificate first if there is none yet.
    Fingerprint,
    /// Probe this server's own `/v1/health` on loopback, trusting only the
    /// certificate it serves; exits 0 when it answers ok. This is the
    /// container healthcheck.
    Health,
    /// Create a single-use invite and print its short code to stdout, then,
    /// when TERMLYX_URL is set, the full code for the same invite.
    Invite {
        /// A note for yourself, e.g. who the invite is for.
        #[arg(long)]
        note: Option<String>,
        /// How long the invite works: a number and a unit, `m`, `h` or `d`
        /// (`30m`, `12h`, `7d`).
        #[arg(long, default_value = "7d", value_parser = parse_duration)]
        expires: i64,
    },
    /// Print the first-account setup code again (only while the server has
    /// no account; exits with status 3 once it has one).
    SetupCode,
    /// Print an add-device code (valid 10 minutes, once) for an account, for
    /// a lone device that lost its session and its device secret.
    Link {
        /// The account's username.
        username: String,
        /// The address your devices use to reach this server (host or
        /// host:port); needed when TERMLYX_URL is not set.
        #[arg(long)]
        url: Option<String>,
    },
    /// Manage accounts.
    Users {
        #[command(subcommand)]
        command: UsersCommand,
    },
    /// Manage an account's devices.
    Devices {
        #[command(subcommand)]
        command: DevicesCommand,
    },
    /// Write a consistent copy of the database to PATH while the server runs
    /// (ciphertext only). Refuses to overwrite an existing file.
    Backup {
        /// Where to write the copy.
        path: PathBuf,
        /// Also write the TLS key and certificate to PATH.tls.tar (mode 0600).
        #[arg(long)]
        with_tls: bool,
    },
    /// Replace the database with the backup at PATH. The server must be
    /// stopped. Writes a new epoch so clients notice the rewind.
    Restore {
        /// The backup file made by `backup`.
        path: PathBuf,
        /// Also restore the TLS key and certificate from PATH.tls.tar.
        #[arg(long)]
        with_tls: bool,
    },
}

#[derive(Debug, Subcommand)]
enum UsersCommand {
    /// List accounts: username, created, disabled, number of devices.
    List,
    /// Block an account: login and refresh answer `forbidden`.
    Disable {
        /// The account's username.
        username: String,
    },
    /// Undo `disable`.
    Enable {
        /// The account's username.
        username: String,
    },
    /// Delete a disabled account and all its data (devices, sessions,
    /// records), freeing its username. Without --yes, only says what it would
    /// delete.
    Delete {
        /// The account's username.
        username: String,
        /// Really delete it.
        #[arg(long)]
        yes: bool,
    },
    /// Let clients older than the newest one that connected write again
    /// (after a bad upgrade): sets the account's schema mark back to 0.
    ResetSchema {
        /// The account's username.
        username: String,
    },
}

#[derive(Debug, Subcommand)]
enum DevicesCommand {
    /// List an account's devices: id, name, platform, last seen, revoked.
    List {
        /// The account's username.
        username: String,
    },
    /// Revoke a device by its id (see `devices list`).
    Revoke {
        /// The device id.
        device_id: String,
    },
}

/// Parse `30m`, `12h` or `7d` into milliseconds. Zero and overflow are errors.
pub fn parse_duration(text: &str) -> Result<i64, String> {
    let text = text.trim();
    let bad = || format!("`{text}` is not a duration like 30m, 12h or 7d");
    let unit = text.chars().last().ok_or_else(bad)?;
    let per_unit: i64 = match unit {
        'm' => 60_000,
        'h' => 3_600_000,
        'd' => 86_400_000,
        _ => return Err(bad()),
    };
    let digits = &text[..text.len() - 1];
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    let n: i64 = digits.parse().map_err(|_| bad())?;
    let ms = n.checked_mul(per_unit).ok_or_else(bad)?;
    if ms == 0 {
        return Err("an invite must last longer than zero".into());
    }
    Ok(ms)
}

/// The certificate pin that goes into codes: self-signed mode only.
fn code_pin(cfg: &Config) -> anyhow::Result<Option<[u8; 32]>> {
    Ok(match cfg.tls {
        TlsMode::SelfSigned => Some(tls::load_or_create(cfg)?.spki_pin()),
        _ => None,
    })
}

/// Encode a join code, refusing one that clients would reject (for example a
/// plain-http public URL): better to fail here than at the invitee. `None`
/// when no public URL is configured.
fn encode_checked(cfg: &Config, kind: CodeKind, token: [u8; 32]) -> anyhow::Result<Option<String>> {
    let Some(url) = cfg.public_url.clone() else {
        return Ok(None);
    };
    let code = JoinCode {
        kind,
        url: url.clone(),
        pin: code_pin(cfg)?,
        token,
    }
    .encode();
    JoinCode::parse(&code)
        .map_err(|e| anyhow::anyhow!("TERMLYX_URL ({url}) cannot go into a code: {e}"))?;
    Ok(Some(code))
}

/// Create an invite in the database under `cfg.data_dir` and return its
/// short code plus, when a public URL is configured, the full `tether1-` code
/// for the same invite: the URL, the certificate pin in self-signed mode
/// (none for `files` or `off`) and the short code's token.
pub async fn invite_codes(
    cfg: &Config,
    note: Option<&str>,
    ttl_ms: i64,
) -> anyhow::Result<(core_proto::ShortCode, Option<String>)> {
    code_pin(cfg)?; // fail before touching the database
    let pool = db::open(&cfg.data_dir).await?;
    let code = accounts::create_short_invite(&pool, system_now_ms(), note, ttl_ms).await;
    pool.close().await;
    let code = code?;
    let full = encode_checked(cfg, CodeKind::Invite, code.token())?;
    Ok((code, full))
}

/// Create an add-device link token for `username` and return its code.
/// `url` (an address as typed by the user) overrides the configured public URL.
pub async fn link_code(cfg: &Config, username: &str, url: Option<&str>) -> anyhow::Result<String> {
    let mut cfg = cfg.clone();
    if let Some(u) = url {
        cfg.public_url =
            Some(core_proto::parse_address(u).map_err(|e| anyhow::anyhow!("--url: {e}"))?);
    }
    let cfg = &cfg;
    if cfg.public_url.is_none() {
        anyhow::bail!(
            "this server has no TERMLYX_URL; pass --url ADDRESS (the address your devices use)"
        );
    }
    let username =
        accounts::normalize_username(username).map_err(|e| anyhow::anyhow!("{}", e.message()))?;
    code_pin(cfg)?;
    let pool = db::open(&cfg.data_dir).await?;
    let made = async {
        let id = admin::account_id(&pool, &username).await?;
        let (token, _) = crate::devices::create_link_token(&pool, &id, system_now_ms())
            .await
            .map_err(|e| anyhow::anyhow!("{}", e.message()))?;
        anyhow::Ok(token)
    }
    .await;
    pool.close().await;
    Ok(encode_checked(cfg, CodeKind::Link, made?)?.expect("public_url checked above"))
}

/// The text `fingerprint` prints: `sha256:<hex>` then the certificate names.
pub fn fingerprint_report(cfg: &Config) -> anyhow::Result<String> {
    let material = tls::load_or_create(cfg)?;
    Ok(format!(
        "{}\nnames: {}\n",
        core_proto::fingerprint_text(&material.spki_pin()),
        material.names().join(", ")
    ))
}

/// `setup-code`'s exit status when the server already has an account.
pub const HAS_ACCOUNT_EXIT: i32 = 3;

fn init_logging() {
    use std::io::IsTerminal;
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    // No colour codes in `docker logs` or the journal.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(std::io::stdout().is_terminal())
        .try_init();
}

/// Entry point used by `main`.
pub async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Serve) {
        Command::Serve => {
            init_logging();
            crate::serve(Config::from_env()?).await
        }
        Command::Fingerprint => {
            print!("{}", fingerprint_report(&Config::from_env()?)?);
            Ok(())
        }
        Command::Health => match crate::health::check(&Config::from_env()?).await {
            Ok(_) => {
                println!("ok");
                Ok(())
            }
            Err(e) => Err(e.context("unhealthy")),
        },
        Command::Invite { note, expires } => {
            let (short, full) =
                invite_codes(&Config::from_env()?, note.as_deref(), expires).await?;
            println!("{short}");
            if let Some(full) = full {
                println!("{full}");
            }
            Ok(())
        }
        Command::SetupCode => {
            let cfg = Config::from_env()?;
            let pool = admin::open_existing(&cfg.data_dir).await?;
            let code = crate::setup::ensure(&pool, system_now_ms()).await;
            pool.close().await;
            match code? {
                Some(code) => {
                    println!("{}", crate::setup::banner(&code));
                    Ok(())
                }
                None => {
                    // Its own status so the installers can tell "updated" from a failure.
                    eprintln!(
                        "this server already has an account; use `termlyx-server invite` for more people"
                    );
                    std::process::exit(HAS_ACCOUNT_EXIT);
                }
            }
        }
        Command::Link { username, url } => {
            let code = link_code(&Config::from_env()?, &username, url.as_deref()).await?;
            println!("{code}");
            Ok(())
        }
        Command::Users { command } => {
            let cfg = Config::from_env()?;
            let text = match command {
                UsersCommand::List => admin::users_list(&cfg).await?,
                UsersCommand::Disable { username } => {
                    admin::set_disabled(&cfg, &username, true).await?
                }
                UsersCommand::Enable { username } => {
                    admin::set_disabled(&cfg, &username, false).await?
                }
                UsersCommand::Delete { username, yes } => {
                    admin::delete_user(&cfg, &username, yes).await?
                }
                UsersCommand::ResetSchema { username } => {
                    admin::reset_schema(&cfg, &username).await?
                }
            };
            print!("{text}");
            Ok(())
        }
        Command::Devices { command } => {
            let cfg = Config::from_env()?;
            let text = match command {
                DevicesCommand::List { username } => admin::devices_list(&cfg, &username).await?,
                DevicesCommand::Revoke { device_id } => {
                    admin::revoke_device(&cfg, &device_id).await?
                }
            };
            print!("{text}");
            Ok(())
        }
        Command::Backup { path, with_tls } => {
            let cfg = Config::from_env()?;
            // Works while the server runs: `VACUUM INTO` reads one snapshot.
            let pool = admin::open_existing(&cfg.data_dir).await?;
            let report = backup::backup(&pool, &cfg.data_dir, &path, with_tls).await;
            pool.close().await;
            println!("{}", report?);
            Ok(())
        }
        Command::Restore { path, with_tls } => {
            let cfg = Config::from_env()?;
            println!("{}", backup::restore(&cfg.data_dir, &path, with_tls).await?);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("30m"), Ok(30 * 60_000));
        assert_eq!(parse_duration("12h"), Ok(12 * 3_600_000));
        assert_eq!(parse_duration("7d"), Ok(7 * 86_400_000));
        assert_eq!(parse_duration(" 1d "), Ok(86_400_000));
        for bad in [
            "",
            "d",
            "5",
            "5x",
            "0m",
            "-1h",
            "1.5h",
            "+3d",
            "1 d",
            "99999999999999999999d",
            "9223372036854775807d",
        ] {
            assert!(parse_duration(bad).is_err(), "{bad:?}");
        }
    }
}
