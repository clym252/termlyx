//! Termlyx desktop app: thin Tauri commands over the core services.
//!
//! * [`state`]: the shared [`AppState`] every command calls into.
//! * [`autolock`]: locks the vault after time away from the front.
//! * [`commands`]: sessions (shells on vault hosts or local ptys).
//! * [`vault_cmds`]: vault lifecycle, entities, import, keys.
//! * [`sync_cmds`]: sync (enable, join, devices, status), password change
//!   support and conflicts.
//! * [`sftp_cmds`], [`forward_cmds`], [`prompt_cmds`]: SFTP and transfers,
//!   port forwards, prompts.
//! * [`keepalive`]: opt-in background keep-alive (iOS, Android) while sessions or
//!   forwards are open.
//! * `android` (Android only): the app's own Kotlin plugin and its commands.
//! * `ios` (iOS only): removes the webview's keyboard form accessory bar;
//!   drives keep-alive's location updates.
//!
//! The TypeScript side is `apps/tauri/src/api/` (types, the real `invoke`
//! wrapper and an in-memory mock); `src/api/bindings.json` lists every
//! DTO's field names and is checked against both sides by tests.

/// The name the app shows users (menus, prompts); ids and crate names keep "tether".
pub const APP_NAME: &str = "Termlyx";

#[cfg(target_os = "android")]
pub mod android;
pub mod autolock;
#[cfg(feature = "bench")]
mod bench;
pub mod commands;
pub mod forward_cmds;
#[cfg(target_os = "ios")]
mod ios;
pub mod keepalive;
#[cfg(desktop)]
pub mod menu;
pub mod migrate;
pub mod prompt_cmds;
pub mod sftp_cmds;
mod sink;
pub mod state;
pub mod sync_cmds;
pub mod vault_cmds;

use std::path::Path;
use std::sync::{Arc, Mutex};

use core_api::vault::VaultService;
use core_ssh::RusshTransport;
use tauri::{AppHandle, Emitter, Manager, Runtime};

pub use sink::{ChannelProgress, ChannelSink, SessionClosed};
pub use state::{AppState, CmdError, EventSink, VaultSlot};

/// File name of the vault database inside the app-data directory.
pub const VAULT_FILE: &str = "vault.sqlite";

/// Log filter when `RUST_LOG` is unset: `info`, with russh capped at `info`
/// (its debug output is per packet). Nothing in this app logs secrets; the
/// third-party HTTP/WebSocket stack is capped separately, see
/// [`with_client_log_caps`].
const DEFAULT_LOG_FILTER: &str = "info,russh=info";

/// Sends events to every webview through the app handle.
struct TauriEvents(AppHandle);

impl EventSink for TauriEvents {
    fn emit(&self, event: &str, payload: serde_json::Value) -> Result<(), String> {
        self.0.emit(event, payload).map_err(|e| e.to_string())
    }
}

/// Caps the sync client's HTTP/WebSocket stack ([`core_sync::LOG_DIRECTIVES`])
/// on top of `filter`. Applied last, so no `RUST_LOG` can lower it: at
/// `trace` tungstenite logs the WebSocket upgrade request, which carries the
/// sync access token.
fn with_client_log_caps(
    mut filter: tracing_subscriber::EnvFilter,
) -> tracing_subscriber::EnvFilter {
    for directive in core_sync::LOG_DIRECTIVES {
        match directive.parse() {
            Ok(d) => filter = filter.add_directive(d),
            Err(e) => tracing::error!(%directive, error = %e, "bad built-in log directive"),
        }
    }
    filter
}

/// Installs the app's log subscriber (stdout, `RUST_LOG` or
/// [`DEFAULT_LOG_FILTER`], then [`core_sync::LOG_DIRECTIVES`] on top).
/// Public for the acceptance canary (`examples/acceptance_canary.rs`), which
/// must log exactly like the app.
pub fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = with_client_log_caps(
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_LOG_FILTER)),
    );
    // Already set (e.g. by a test harness): keep that one.
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
}

/// Opens the vault at `path`. A vault that cannot be opened (unreadable
/// directory, damaged file) does not stop the app: it starts with the
/// vault unavailable and `vault_status` says why. The file is left as is.
/// Public for the acceptance canary, which opens its vault the same way.
/// `gate` stands in front of quick unlock (`default_gate()`, or on Android
/// the Keystore plugin's).
pub fn open_vault(
    dir: Result<std::path::PathBuf, String>,
    gate: Box<dyn core_api::vault::PresenceGate>,
) -> VaultSlot {
    let opened = dir.and_then(|dir| {
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
        #[cfg(unix)]
        {
            // Owner only: the vault and its backups live here.
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        }
        let path = dir.join(VAULT_FILE);
        VaultService::open(Path::new(&path))
            .map(|v| v.with_gate(gate))
            .map_err(|e| format!("could not open the vault at {}: {e}", path.display()))
    });
    match opened {
        Ok(vault) => VaultSlot::Ready(Arc::new(Mutex::new(vault))),
        Err(why) => {
            tracing::error!(error = %why, "vault unavailable; starting without it");
            VaultSlot::Failed(why)
        }
    }
}

/// Tells the page which OS it runs on before any page script: the frontend
/// cannot (iPadOS WKWebView reports `navigator.platform` as `MacIntel`).
fn platform_script() -> String {
    format!(
        "window.__TETHER_OS__ = {};",
        serde_json::Value::from(std::env::consts::OS)
    )
}

/// A plugin with no commands that only injects [`platform_script`].
fn platform_plugin<R: Runtime>() -> tauri::plugin::TauriPlugin<R> {
    tauri::plugin::Builder::new("tether-platform")
        .js_init_script(platform_script())
        .build()
}

/// The command list, plus the bench commands when built with `bench`.
macro_rules! handlers {
    ($($extra:path),* $(,)?) => {
        tauri::generate_handler![
            commands::open_session,
            commands::write_shell,
            commands::resize_shell,
            commands::ack_shell,
            commands::close_shell,
            commands::close_all_sessions,
            commands::active_work,
            // No local terminal on mobile.
            #[cfg(desktop)]
            commands::local_shells,
            commands::session_tmux,
            vault_cmds::vault_status,
            vault_cmds::vault_create,
            vault_cmds::vault_unlock,
            vault_cmds::vault_unlock_recovery,
            vault_cmds::vault_lock,
            vault_cmds::vault_enable_quick_unlock,
            vault_cmds::vault_disable_quick_unlock,
            vault_cmds::vault_quick_unlock,
            autolock::set_auto_lock,
            autolock::app_front,
            sync_cmds::set_device_fallback,
            vault_cmds::list_entities,
            vault_cmds::upsert_entity,
            vault_cmds::delete_entity,
            vault_cmds::delete_group,
            vault_cmds::set_own_login,
            vault_cmds::effective_settings,
            vault_cmds::import_ssh_config,
            vault_cmds::key_generate,
            vault_cmds::key_import,
            vault_cmds::key_export_private,
            vault_cmds::key_save_private,
            vault_cmds::known_hosts_import,
            sync_cmds::sync_status,
            sync_cmds::sync_parse_code,
            sync_cmds::sync_check_server,
            sync_cmds::sync_enable,
            sync_cmds::sync_join,
            sync_cmds::sync_recover,
            sync_cmds::sync_now,
            sync_cmds::sync_pause,
            sync_cmds::sync_sign_in,
            sync_cmds::sync_turn_off,
            sync_cmds::sync_link_code,
            sync_cmds::sync_devices,
            sync_cmds::sync_rename_device,
            sync_cmds::sync_revoke_device,
            sync_cmds::vault_change_password,
            sync_cmds::conflicts_list,
            sync_cmds::conflicts_resolve,
            sync_cmds::conflicts_reveal,
            sftp_cmds::sftp_open,
            sftp_cmds::sftp_close,
            sftp_cmds::sftp_list,
            sftp_cmds::sftp_mkdir,
            sftp_cmds::sftp_rename,
            sftp_cmds::sftp_remove,
            sftp_cmds::sftp_chmod,
            sftp_cmds::transfer_enqueue,
            sftp_cmds::transfer_cancel,
            sftp_cmds::local_list,
            sftp_cmds::local_mkdir,
            sftp_cmds::local_resolve,
            sftp_cmds::local_drives,
            sftp_cmds::local_import,
            sftp_cmds::sftp_realpath,
            forward_cmds::forward_start,
            forward_cmds::forward_stop,
            forward_cmds::forward_statuses,
            keepalive::keepalive_set,
            keepalive::keepalive_status,
            prompt_cmds::prompt_respond,
            // Android only: Back on Hosts goes to the background; the device file picker.
            #[cfg(target_os = "android")]
            android::move_to_background,
            #[cfg(target_os = "android")]
            android::local_import_from_device,
            #[cfg(target_os = "android")]
            android::local_save_to_device,
            $($extra),*
        ]
    };
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    init_tracing();
    let context = tauri::generate_context!();
    // Before any window: a webview would create the new id's data dir, and
    // the old device settings would then stay behind (src/migrate.rs).
    #[cfg(desktop)]
    if let Err(m) = migrate::run(&context.config().identifier) {
        migrate::fail(&m)
    }
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(platform_plugin())
        .setup(|app| {
            // The terminal's key bar takes the place of WKWebView's prev/next/Done bar
            // and (iPad) the shortcut strip.
            #[cfg(target_os = "ios")]
            {
                ios::hide_form_accessory_bar();
                ios::hide_shortcut_strip();
            }
            // The local pane of SFTP is Tether's Documents folder, shown in Files.
            #[cfg(target_os = "ios")]
            sftp_cmds::restrict_local_to(app.path().document_dir().map_err(|e| e.to_string())?);
            // On Android it is the app's private files folder.
            #[cfg(target_os = "android")]
            sftp_cmds::restrict_local_to(
                app.path()
                    .app_data_dir()
                    .map_err(|e| e.to_string())?
                    .join("files"),
            );
            let events: Arc<dyn EventSink> = Arc::new(TauriEvents(app.handle().clone()));
            // Android: biometric quick unlock through the plugin (registered
            // before setup runs); never without a gate.
            #[cfg(target_os = "android")]
            let gate = android::quick_unlock_gate(app.handle());
            #[cfg(not(target_os = "android"))]
            let gate = core_api::vault::default_gate();
            let vault = open_vault(app.path().app_data_dir().map_err(|e| e.to_string()), gate);
            let prompts = Arc::new(prompt_cmds::prompt_bridge(events.clone()));
            let state = AppState::new(vault, prompts, events, Arc::new(RusshTransport));
            app.manage(state.clone());
            // Background keep-alive's location manager (made here, on the main thread).
            #[cfg(target_os = "ios")]
            ios::keepalive::install(state.keepalive());
            // Android: the same slot, driven by a foreground service.
            #[cfg(target_os = "android")]
            android::install_keepalive(app.handle(), &state);
            // Auto-lock limit Never until the webview sends the device's
            // choice (the vault starts locked, so nothing waits on it).
            autolock::spawn_timer(state.clone());
            // Windows Hello prompts open over the main window (it exists by
            // now: config windows are built before setup runs).
            #[cfg(windows)]
            if let Some(window) = app.get_webview_window("main") {
                match window.hwnd() {
                    Ok(hwnd) => core_api::vault::set_prompt_owner(hwnd.0 as isize),
                    Err(e) => tracing::warn!("no window handle for Windows Hello prompts: {e}"),
                }
            }
            // A device that syncs keeps syncing while locked (it picks up a
            // password changed elsewhere); the engine starts with the app.
            tauri::async_runtime::spawn(async move { state.start_sync().await });
            Ok(())
        })
        // Window focus (desktop; on iOS the scene becoming or resigning
        // active) drives the auto-lock natively, before the webview reacts.
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::Focused(focused) = event {
                autolock::on_focus(window.app_handle(), *focused);
                // Back in front: a hardware keyboard's shortcuts work without a tap.
                #[cfg(target_os = "ios")]
                if *focused && let Some(main) = window.app_handle().get_webview_window("main") {
                    ios::claim_hardware_keys(main.as_ref());
                }
            }
        });
    // iOS: the same once the page has loaded (launch).
    #[cfg(target_os = "ios")]
    let builder = builder.on_page_load(|webview, payload| {
        if payload.event() == tauri::webview::PageLoadEvent::Finished {
            ios::claim_hardware_keys(webview);
        }
    });
    // Desktop: signed self-update; Settings → About and the notice after unlock.
    #[cfg(desktop)]
    let builder = builder
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init());
    // Mobile: the camera scanner for "Scan QR code" when joining a synced vault.
    #[cfg(mobile)]
    let builder = builder.plugin(tauri_plugin_barcode_scanner::init());
    // Mobile: the terminal's long-press Copy and Paste (iOS: UIPasteboard).
    #[cfg(mobile)]
    let builder = builder.plugin(tauri_plugin_clipboard_manager::init());
    // Android: the app's own plugin (`TetherPlugin.kt`).
    #[cfg(target_os = "android")]
    let builder = builder.plugin(android::init());
    // macOS: Quit and Close Window go through the guarded close request.
    #[cfg(target_os = "macos")]
    let builder = builder.menu(menu::build).on_menu_event(menu::on_event);
    // `bench` implies `dev-connect` (Cargo.toml).
    #[cfg(feature = "bench")]
    let builder = builder.invoke_handler(handlers![
        commands::open_shell,
        bench::bench_config,
        bench::bench_report
    ]);
    #[cfg(all(feature = "dev-connect", not(feature = "bench")))]
    let builder = builder.invoke_handler(handlers![commands::open_shell]);
    #[cfg(not(feature = "dev-connect"))]
    let builder = builder.invoke_handler(handlers![]);
    builder
        .build(context)
        .expect("error while building tauri application")
        .run(|app, event| {
            // Quit: let the sync engine finish its step and stop.
            if matches!(event, tauri::RunEvent::Exit)
                && let Some(state) = app.try_state::<AppState>()
            {
                tauri::async_runtime::block_on(state.stop_sync());
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Default)]
    struct Capture(Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for Capture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
        type Writer = Capture;
        fn make_writer(&'a self) -> Capture {
            self.clone()
        }
    }

    /// `RUST_LOG=trace` must not lower the client stack's caps: tungstenite
    /// traces the upgrade request with the access token in it.
    #[test]
    fn rust_log_trace_cannot_lift_the_client_log_caps() {
        let capture = Capture::default();
        let filter = with_client_log_caps(tracing_subscriber::EnvFilter::new("trace"));
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(capture.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            tracing::trace!(target: "tungstenite::handshake::client", "LEAK bearer.token");
            tracing::trace!(target: "reqwest::connect::verbose", "LEAK raw bytes");
            tracing::trace!(target: "hyper::proto::h1::io", "LEAK raw bytes");
            tracing::trace!(target: "tether_app", "kept");
        });
        let text = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
        assert!(!text.contains("LEAK"), "{text}");
        assert!(text.contains("kept"), "{text}");
    }

    #[test]
    fn platform_script_names_this_os() {
        assert_eq!(
            platform_script(),
            format!("window.__TETHER_OS__ = \"{}\";", std::env::consts::OS)
        );
    }

    #[test]
    fn unopenable_vault_is_reported_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let garbage = dir.path().join(VAULT_FILE);
        std::fs::write(
            &garbage,
            b"this is not a sqlite database, not even close....",
        )
        .unwrap();
        let before = std::fs::read(&garbage).unwrap();
        let VaultSlot::Failed(why) = open_vault(
            Ok(dir.path().to_path_buf()),
            core_api::vault::default_gate(),
        ) else {
            panic!("a garbage vault file must not open");
        };
        assert!(why.contains("could not open the vault"), "{why}");
        // Left untouched.
        assert_eq!(std::fs::read(&garbage).unwrap(), before);

        let VaultSlot::Failed(why) = open_vault(
            Err("no app data dir".into()),
            core_api::vault::default_gate(),
        ) else {
            panic!("no directory, no vault");
        };
        assert_eq!(why, "no app data dir");
    }

    /// Every command the handler list registers must be declared in the
    /// build script's app manifest and granted in the default capability,
    /// or the webview is denied it at runtime (invisible to the browser
    /// mock and to command-layer tests). Optional commands (dev-connect,
    /// bench) are passed to `handlers!` by the `run` builder, outside the
    /// macro body, and have their own capability files.
    /// A command name as `handlers!` lists it: `[a-z0-9_]+`.
    fn is_command_name(n: &str) -> bool {
        !n.is_empty()
            && n.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    }

    #[test]
    fn command_names_may_have_digits() {
        for name in ["open_session", "sftp_list2", "v2_sync"] {
            assert!(is_command_name(name), "{name}");
        }
        for not in ["", "Foo", "a-b", "x.y", "$($extra)"] {
            assert!(!is_command_name(not), "{not}");
        }
    }

    #[test]
    fn registered_commands_are_declared_and_granted() {
        use std::collections::BTreeSet;

        let lib = include_str!("lib.rs");
        let body = lib
            .split("tauri::generate_handler![")
            .nth(1)
            .and_then(|rest| rest.split("$($extra),*").next())
            .expect("the handlers! macro body");
        let registered: BTreeSet<String> = body
            .lines()
            .filter_map(|l| {
                l.trim()
                    .strip_suffix(',')?
                    .rsplit("::")
                    .next()
                    .map(str::to_owned)
            })
            .filter(|n| is_command_name(n))
            .collect();
        assert!(
            registered.len() > 50,
            "parsed {} commands",
            registered.len()
        );

        let build = include_str!("../build.rs");
        let declared: BTreeSet<String> = build
            .split("const COMMANDS")
            .nth(1)
            .and_then(|rest| rest.split("];").next())
            .expect("the COMMANDS list")
            .lines()
            .filter_map(|l| {
                let name = l.trim().strip_prefix('"')?.strip_suffix("\",")?;
                Some(name.to_owned())
            })
            .collect();

        let capability: serde_json::Value =
            serde_json::from_str(include_str!("../capabilities/default.json")).unwrap();
        let granted: BTreeSet<String> = capability["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|p| p.as_str()?.strip_prefix("allow-"))
            .map(|p| p.replace('-', "_"))
            .collect();

        let missing = |a: &BTreeSet<String>, b: &BTreeSet<String>| {
            a.difference(b).cloned().collect::<Vec<_>>()
        };
        assert_eq!(
            missing(&registered, &declared),
            Vec::<String>::new(),
            "registered but not in build.rs COMMANDS"
        );
        assert_eq!(
            missing(&declared, &registered),
            Vec::<String>::new(),
            "in build.rs COMMANDS but not registered"
        );
        assert_eq!(
            missing(&registered, &granted),
            Vec::<String>::new(),
            "registered but not allowed in capabilities/default.json"
        );
        assert_eq!(
            missing(&granted, &registered),
            Vec::<String>::new(),
            "allowed in capabilities/default.json but not registered"
        );
    }
}
