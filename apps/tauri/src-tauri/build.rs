use std::path::{Path, PathBuf};

/// Commands the webview may call (each gets an `allow-<name>` permission,
/// granted in `capabilities/default.json`).
const COMMANDS: &[&str] = &[
    // sessions
    "open_session",
    "write_shell",
    "resize_shell",
    "ack_shell",
    "close_shell",
    "close_all_sessions",
    "active_work",
    "local_shells",
    "session_tmux",
    // vault
    "vault_status",
    "vault_create",
    "vault_unlock",
    "vault_unlock_recovery",
    "vault_lock",
    "vault_enable_quick_unlock",
    "vault_disable_quick_unlock",
    "vault_quick_unlock",
    "set_auto_lock",
    "app_front",
    "set_device_fallback",
    "list_entities",
    "upsert_entity",
    "delete_entity",
    "delete_group",
    "set_own_login",
    "effective_settings",
    "import_ssh_config",
    "key_generate",
    "key_import",
    "key_export_private",
    "known_hosts_import",
    // sync, password change, conflicts
    "sync_status",
    "sync_parse_code",
    "sync_check_server",
    "sync_enable",
    "sync_join",
    "sync_recover",
    "sync_now",
    "sync_pause",
    "sync_sign_in",
    "sync_turn_off",
    "sync_link_code",
    "sync_devices",
    "sync_rename_device",
    "sync_revoke_device",
    "vault_change_password",
    "conflicts_list",
    "conflicts_resolve",
    "conflicts_reveal",
    // sftp
    "sftp_open",
    "sftp_close",
    "sftp_list",
    "sftp_mkdir",
    "sftp_rename",
    "sftp_remove",
    "sftp_chmod",
    "transfer_enqueue",
    "transfer_cancel",
    "local_list",
    "local_mkdir",
    "local_resolve",
    "local_drives",
    "local_import",
    "sftp_realpath",
    // forwards
    "forward_start",
    "forward_stop",
    "forward_statuses",
    // background keep-alive (iOS)
    "keepalive_set",
    "keepalive_status",
    // prompts
    "prompt_respond",
    // Android: Back on Hosts goes to the background; the device file picker
    "move_to_background",
    "local_import_from_device",
    "local_save_to_device",
];

/// Optional command sets: (cargo feature env var, commands, capability file
/// granting them). A disabled feature's commands are neither declared nor
/// granted, so the webview cannot call them.
const OPTIONAL: &[(&str, &[&str], &str)] = &[
    (
        "CARGO_FEATURE_DEV_CONNECT",
        &["open_shell"],
        "capabilities/dev-connect/dev-connect.json",
    ),
    (
        "CARGO_FEATURE_BENCH",
        &["bench_config", "bench_report"],
        "capabilities/bench/bench.json",
    ),
];

const DEFAULT_CAPABILITY: &str = "capabilities/default.json";

/// Granted on iOS and Android only: the plugins (and so their permissions)
/// exist only there.
const MOBILE_CAPABILITY: &str = "capabilities/mobile.json";

/// Granted on desktop only, the mirror of the mobile one (the updater and
/// process plugins).
const DESKTOP_CAPABILITY: &str = "capabilities/desktop.json";

fn main() {
    let mut commands = COMMANDS.to_vec();
    let mut capabilities = vec![DEFAULT_CAPABILITY];
    for (feature, extra, capability) in OPTIONAL {
        if std::env::var_os(feature).is_some() {
            commands.extend_from_slice(extra);
            capabilities.push(capability);
        }
    }

    if matches!(
        std::env::var("CARGO_CFG_TARGET_OS").as_deref(),
        Ok("ios" | "android")
    ) {
        capabilities.push(MOBILE_CAPABILITY);
    } else {
        capabilities.push(DESKTOP_CAPABILITY);
    }

    // tauri-build takes one glob for capability files. The enabled ones are
    // copied into OUT_DIR and the glob points there, so each feature's
    // capability is picked up only when the feature is on.
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR")).join("capabilities");
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out).expect("create capabilities dir in OUT_DIR");
    for capability in &capabilities {
        let src = Path::new(capability);
        std::fs::copy(src, out.join(src.file_name().expect("file name")))
            .unwrap_or_else(|e| panic!("copy {capability}: {e}"));
    }
    // tauri-build does not watch the files when given a pattern.
    println!("cargo:rerun-if-changed=capabilities");
    let pattern: &'static str = format!("{}/*.json", out.display()).leak();

    // Least privilege: declare our commands in an app manifest so the
    // capability can grant exactly these (and nothing from core/plugins).
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .capabilities_path_pattern(pattern)
            .app_manifest(tauri_build::AppManifest::new().commands(commands.leak())),
    )
    .expect("failed to run tauri-build");
}
