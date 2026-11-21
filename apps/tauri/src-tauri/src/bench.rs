//! Throughput-spike harness (Phase 0). Entirely inert unless the app is
//! launched with `TETHER_BENCH=1`: `bench_config` then returns `None` and
//! `bench_report` refuses to run.
//!
//! In bench mode the UI auto-connects to the SSH fixture, pushes ~135 MB
//! through the terminal, and hands the measurements to `bench_report`, which
//! appends them to `docs/spikes/throughput-results.json` and exits the app.

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Map, Value, json};
use tauri::AppHandle;

const RESULTS_REL_PATH: &str = "docs/spikes/throughput-results.json";

fn enabled() -> bool {
    std::env::var("TETHER_BENCH").as_deref() == Ok("1")
}

/// What the UI needs to run the bench. The password comes only from the
/// environment (`TETHER_BENCH_PASSWORD`); nothing is hardcoded.
#[derive(Serialize)]
pub struct BenchConfig {
    host: String,
    port: u16,
    username: String,
    password: String,
}

fn config_from_env(get: impl Fn(&str) -> Option<String>) -> Option<BenchConfig> {
    if get("TETHER_BENCH").as_deref() != Some("1") {
        return None;
    }
    let Some(password) = get("TETHER_BENCH_PASSWORD") else {
        eprintln!("TETHER_BENCH=1 but TETHER_BENCH_PASSWORD is not set; bench disabled");
        return None;
    };
    Some(BenchConfig {
        host: get("TETHER_BENCH_HOST").unwrap_or_else(|| "127.0.0.1".into()),
        port: get("TETHER_BENCH_PORT")
            .and_then(|p| p.parse().ok())
            .unwrap_or(2222),
        username: get("TETHER_BENCH_USER").unwrap_or_else(|| "tether".into()),
        password,
    })
}

#[tauri::command]
pub fn bench_config() -> Option<BenchConfig> {
    config_from_env(|k| std::env::var(k).ok())
}

fn repo_root() -> PathBuf {
    match std::env::var_os("TETHER_REPO_ROOT") {
        Some(p) => PathBuf::from(p),
        None => Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."),
    }
}

/// Adds build facts the UI cannot know, then appends the run to `file`
/// (a JSON array; a missing file starts a new array).
fn append_result(file: &Path, report: &str) -> Result<(), String> {
    let mut run: Map<String, Value> =
        serde_json::from_str(report).map_err(|e| format!("bad report json: {e}"))?;
    run.insert(
        "build".into(),
        json!(if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }),
    );
    run.insert("os".into(), json!(std::env::consts::OS));
    run.insert("arch".into(), json!(std::env::consts::ARCH));
    run.insert("frameBytes".into(), json!(core_api::MAX_FRAME_BYTES));
    run.insert(
        "frameIntervalMs".into(),
        json!(core_api::FRAME_INTERVAL.as_millis() as u64),
    );
    run.insert(
        "unackedHighWatermark".into(),
        json!(core_api::UNACKED_HIGH_WATERMARK),
    );

    let mut runs: Vec<Value> = match std::fs::read_to_string(file) {
        Ok(s) => serde_json::from_str(&s)
            .map_err(|e| format!("{} is not a JSON array: {e}", file.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e.to_string()),
    };
    runs.push(Value::Object(run));
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let out = serde_json::to_string_pretty(&runs).map_err(|e| e.to_string())?;
    std::fs::write(file, out + "\n").map_err(|e| e.to_string())
}

/// Records one bench run and exits the app (bench mode only).
#[tauri::command]
pub fn bench_report(app: AppHandle, json: String) -> Result<(), String> {
    if !enabled() {
        return Err("bench mode is not enabled".into());
    }
    let result = append_result(&repo_root().join(RESULTS_REL_PATH), &json);
    if let Err(e) = &result {
        eprintln!("bench_report failed: {e}");
    }
    app.exit(if result.is_ok() { 0 } else { 1 });
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).into(), (*v).into()))
            .collect();
        move |k| map.get(k).cloned()
    }

    #[test]
    fn inert_without_flag_or_password() {
        assert!(config_from_env(env(&[])).is_none());
        assert!(config_from_env(env(&[("TETHER_BENCH_PASSWORD", "x")])).is_none());
        assert!(
            config_from_env(env(&[
                ("TETHER_BENCH", "0"),
                ("TETHER_BENCH_PASSWORD", "x")
            ]))
            .is_none()
        );
        assert!(config_from_env(env(&[("TETHER_BENCH", "1")])).is_none());
    }

    #[test]
    fn defaults_to_the_fixture() {
        let c = config_from_env(env(&[
            ("TETHER_BENCH", "1"),
            ("TETHER_BENCH_PASSWORD", "pw"),
        ]))
        .unwrap();
        assert_eq!(
            (c.host.as_str(), c.port, c.username.as_str()),
            ("127.0.0.1", 2222, "tether")
        );
    }

    #[test]
    fn appends_runs_to_a_json_array() {
        let dir = std::env::temp_dir().join(format!("tether-bench-test-{}", std::process::id()));
        let file = dir.join("nested/results.json");
        append_result(&file, r#"{"mbps":1.5}"#).unwrap();
        append_result(&file, r#"{"mbps":2.5}"#).unwrap();
        let runs: Vec<Value> =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[1]["mbps"], 2.5);
        assert!(runs[0]["build"].is_string() && runs[0]["frameBytes"].is_number());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
