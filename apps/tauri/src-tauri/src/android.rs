//! Android-only: the app's own Tauri plugin, `TetherPlugin` (Kotlin,
//! `gen/android/app/src/main/java/ca/clym/termlyx/TetherPlugin.kt`).
//!
//! [`init`] registers it and keeps its [`PluginHandle`] in managed state as
//! [`TetherPlugin`]; each native feature is a `#[tauri::command]` here that
//! calls the matching `@Command` of the Kotlin class through the handle, or
//! (biometric quick unlock) a core trait the vault calls: [`quick_unlock_gate`],
//! or (background keep-alive) the [`ServiceDriver`] the keep-alive slot drives.

use std::sync::Arc;

use core_api::vault::{KeystoreBackend, KeystoreError, KeystoreGate, PresenceGate, default_gate};
use data_encoding::BASE64;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tauri::plugin::mobile::PluginInvokeError;
use tauri::plugin::{Builder, PluginHandle, TauriPlugin};
use tauri::{AppHandle, Manager, Runtime, State, Wry};
use zeroize::Zeroizing;

use crate::CmdError;
use crate::keepalive::LocationDriver;
use crate::sftp_cmds;
use crate::state::AppState;

/// Package and class of the Kotlin plugin.
const PACKAGE: &str = "ca.clym.termlyx";
const CLASS: &str = "TetherPlugin";

/// The Kotlin plugin's handle.
pub struct TetherPlugin<R: Runtime>(pub PluginHandle<R>);

impl<R: Runtime> TetherPlugin<R> {
    /// Runs `command` on the Kotlin plugin.
    pub fn run<T: DeserializeOwned>(&self, command: &str) -> Result<T, CmdError> {
        self.0
            .run_mobile_plugin(command, ())
            .map_err(|e| CmdError::new("plugin", e.to_string()))
    }
}

/// The plugin to add to the builder: registers the Kotlin class and manages its handle.
pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("tether")
        .setup(|app, api| {
            app.manage(TetherPlugin(api.register_android_plugin(PACKAGE, CLASS)?));
            Ok(())
        })
        .build()
}

/// Sends the app to the background, as Home does: Back on Hosts with nothing
/// open never closes it, and sessions keep running.
#[tauri::command]
pub fn move_to_background(plugin: State<'_, TetherPlugin<Wry>>) -> Result<(), CmdError> {
    plugin.run("background")
}

/// What the Kotlin side answers to `importFiles`: the names the files got.
#[derive(serde::Deserialize)]
struct Imported {
    names: Vec<String>,
}

#[derive(serde::Deserialize)]
struct Saved {
    saved: bool,
}

#[derive(Serialize)]
struct DirArgs {
    dir: String,
}

#[derive(Serialize)]
struct PathArgs {
    path: String,
}

/// Runs a plugin command that waits on the user (a system picker): no time
/// limit, on a blocking thread, holding no lock. A refusal keeps the Kotlin
/// side's sentence for the user.
async fn ask_user<A, T>(plugin: &TetherPlugin<Wry>, command: &'static str, args: A) -> Result<T, CmdError>
where
    A: Serialize + Send + 'static,
    T: DeserializeOwned + Send + 'static,
{
    let handle = plugin.0.clone();
    tauri::async_runtime::spawn_blocking(move || handle.run_mobile_plugin::<T>(command, args))
        .await
        .map_err(|e| CmdError::internal(e.to_string()))?
        .map_err(|e| match e {
            PluginInvokeError::InvokeRejected(r) => CmdError::io(r.message.unwrap_or_else(|| "The file picker failed.".into())),
            _ => CmdError::io("The file picker gave no usable answer."),
        })
}

/// "Import from device": the system picker (several files), each copied into
/// `dir`, a folder inside Tether's folder. Returns the names they got; none
/// when the picker was cancelled.
#[tauri::command]
pub async fn local_import_from_device(
    plugin: State<'_, TetherPlugin<Wry>>,
    dir: String,
) -> Result<Vec<String>, CmdError> {
    let dir = sftp_cmds::plugin_path(&dir, sftp_cmds::Want::Dir)?;
    Ok(ask_user::<_, Imported>(&plugin, "importFiles", DirArgs { dir }).await?.names)
}

/// "Save to device…": the system "save as" picker, then `path`, a file inside
/// Tether's folder, is written to the place chosen. False when cancelled.
#[tauri::command]
pub async fn local_save_to_device(plugin: State<'_, TetherPlugin<Wry>>, path: String) -> Result<bool, CmdError> {
    let path = sftp_cmds::plugin_path(&path, sftp_cmds::Want::File)?;
    Ok(ask_user::<_, Saved>(&plugin, "exportFile", PathArgs { path }).await?.saved)
}

/// The Kotlin plugin as the Android Keystore of quick unlock
/// (`biometricHmac`). Called from the vault's blocking threads, never the
/// main thread, which the prompt needs.
pub struct PluginBackend(pub PluginHandle<Wry>);

#[derive(Serialize)]
struct HmacArgs {
    challenge: String,
    reason: String,
    create: bool,
}

/// The MAC, base64. Not `Debug`: it is key material.
#[derive(serde::Deserialize)]
struct HmacReply {
    mac: String,
}

const NOT_NOW: &str = "Biometric unlock is not available right now.";

/// How long a biometric prompt may take before it counts as unavailable.
const HMAC_WAIT: std::time::Duration = std::time::Duration::from_secs(120);

impl KeystoreBackend for PluginBackend {
    fn hmac(
        &self,
        challenge: &[u8],
        reason: &str,
        create: bool,
    ) -> Result<Zeroizing<Vec<u8>>, KeystoreError> {
        let args = HmacArgs {
            challenge: BASE64.encode(challenge),
            reason: reason.to_owned(),
            create,
        };
        // The vault calls this inside `with_vault`, its lock held: an invoke
        // the Kotlin side never answers must not wedge the app, so the wait is
        // bounded, far beyond any human prompt.
        let plugin = self.0.clone();
        let reply: HmacReply = bounded(HMAC_WAIT, move || {
            Ok(plugin.run_mobile_plugin::<HmacReply>("biometricHmac", args))
        })
        .map_err(|e| {
            tracing::warn!("biometric quick unlock: {e}");
            KeystoreError::Unavailable(NOT_NOW.into())
        })?
        .map_err(keystore_error)?;
        let mac = Zeroizing::new(reply.mac);
        BASE64
            .decode(mac.as_bytes())
            .map(Zeroizing::new)
            .map_err(|_| KeystoreError::Unavailable(NOT_NOW.into()))
    }
}

/// By reject code, never by message text; an `unavailable` message is the
/// Kotlin side's sentence for the user.
fn keystore_error(e: PluginInvokeError) -> KeystoreError {
    match e {
        PluginInvokeError::InvokeRejected(r) => match r.code.as_deref() {
            Some("cancelled") => KeystoreError::Cancelled,
            Some("invalidated") => KeystoreError::Invalidated,
            Some("missing") => KeystoreError::Missing,
            _ => KeystoreError::Unavailable(r.message.unwrap_or_else(|| NOT_NOW.into())),
        },
        // Not the error's text: a response that failed to parse may quote it.
        _ => {
            tracing::warn!("biometric quick unlock: no usable answer from the plugin");
            KeystoreError::Unavailable(NOT_NOW.into())
        }
    }
}

/// Quick unlock's gate on Android: the Keystore through the plugin. Without
/// the plugin it is [`default_gate`], which is unavailable, never `NoGate`.
pub fn quick_unlock_gate(app: &AppHandle<Wry>) -> Box<dyn PresenceGate> {
    match app.try_state::<TetherPlugin<Wry>>() {
        Some(plugin) => Box::new(KeystoreGate(PluginBackend(plugin.0.clone()))),
        None => {
            tracing::error!("Tether plugin not registered: biometric unlock unavailable");
            default_gate()
        }
    }
}

/// Background keep-alive on Android: the Kotlin plugin's foreground service
/// (`KeepAliveService`) runs while the slot says to, with a notification
/// that counts the open sessions and forwards. Never touches the vault.
///
/// `run_mobile_plugin` needs the Activity (tauri/wry panic with "no
/// available activity" once it is destroyed, e.g. Tether swiped from recents
/// while sessions keep the process alive) and waits for the Kotlin answer
/// with no limit. The slot calls the driver with its lock held, so every
/// plugin call here runs [`bounded`], and the stop, which must work with no
/// Activity (a notification left with no session), goes straight to
/// `KeepAliveService.stop` over JNI on the application context.
struct ServiceDriver {
    plugin: PluginHandle<Wry>,
    /// The live count, for a start (the slot tells the driver only changes).
    live: Box<dyn Fn() -> usize + Send + Sync>,
}

/// `generation` is the slot's: Kotlin drops a call older than the newest
/// start or stop it has seen (a bounded call can land late).
#[derive(Serialize)]
struct KeepAliveArgs {
    count: usize,
    generation: u64,
}

#[derive(Serialize)]
struct StopArgs {
    generation: u64,
}

/// How long the slot's lock may wait for the Kotlin side.
const PLUGIN_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// Runs `f` on its own thread and waits `wait` for it; a panic in it or a
/// timeout is an error (the thread finishes by itself, late).
fn bounded<T: Send + 'static>(
    wait: std::time::Duration,
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let done = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
            .unwrap_or_else(|_| Err("panicked".into()));
        let _ = tx.send(done);
    });
    rx.recv_timeout(wait)
        .unwrap_or_else(|_| Err("no answer".into()))
}

/// `KeepAliveService.stop(context, generation)` on the application context
/// (`Context.stopService` unless a newer start was seen): needs neither the
/// Activity nor the plugin.
fn stop_service_directly(generation: u64) -> Result<(), String> {
    use jni::objects::{JClass, JObject, JValue};
    let ctx = ndk_context::android_context();
    // SAFETY: tao stored the VM pointer and a global reference to the
    // application context at launch; both live as long as the process.
    let vm = unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) }.map_err(|e| e.to_string())?;
    let context = unsafe { JObject::from_raw(ctx.context().cast()) };
    let mut env = vm.attach_current_thread().map_err(|e| e.to_string())?;
    let mut call = || -> jni::errors::Result<()> {
        // Through the app's class loader: `FindClass` on a thread attached
        // from native code sees only the system's classes.
        let name = env.new_string(format!("{PACKAGE}.KeepAliveService"))?;
        let loader = env
            .call_method(&context, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])?
            .l()?;
        let class = env
            .call_method(
                &loader,
                "loadClass",
                "(Ljava/lang/String;)Ljava/lang/Class;",
                &[JValue::Object(&name)],
            )?
            .l()?;
        env.call_static_method(
            JClass::from(class),
            "stop",
            "(Landroid/content/Context;J)V",
            &[JValue::Object(&context), JValue::Long(generation as i64)],
        )?;
        Ok(())
    };
    let result = call();
    if result.is_err() {
        // Leave no pending Java exception on the attached thread.
        let _ = env.exception_clear();
    }
    result.map_err(|e| e.to_string())
}

impl ServiceDriver {
    /// A plugin command, bounded; failures are logged and the next change retries.
    fn command<A: Serialize + Send + 'static>(&self, command: &'static str, args: A) {
        let plugin = self.plugin.clone();
        let result = bounded(PLUGIN_WAIT, move || {
            plugin
                .run_mobile_plugin::<()>(command, args)
                .map_err(|e| e.to_string())
        });
        if let Err(e) = result {
            tracing::warn!("keep-alive service ({command}): {e}");
        }
    }
}

impl LocationDriver for ServiceDriver {
    /// Never asked: the slot is installed as already permitted.
    fn request_permission(&self) {}

    fn set_updating(&self, on: bool, generation: u64) {
        if on {
            let count = (self.live)();
            self.command("keepAlive", KeepAliveArgs { count, generation });
        } else if let Err(e) = bounded(PLUGIN_WAIT, move || stop_service_directly(generation)) {
            // The plugin path needs the Activity; it is the fallback.
            tracing::warn!("keep-alive service not stopped directly ({e}); asking the plugin");
            self.command("keepAliveStop", StopArgs { generation });
        }
    }

    /// The count is cosmetic: with no Activity it is simply not refreshed.
    fn live_changed(&self, live: usize, generation: u64) {
        self.command("keepAliveCount", KeepAliveArgs { count: live, generation });
    }
}

/// Installs the foreground-service driver into the keep-alive slot (called
/// at setup, after the state exists). Without the plugin keep-alive cannot
/// be turned on.
pub fn install_keepalive(app: &AppHandle<Wry>, state: &AppState) {
    let Some(plugin) = app.try_state::<TetherPlugin<Wry>>() else {
        tracing::error!("Tether plugin not registered: background keep-alive unavailable");
        return;
    };
    // shortcut: the driver holds the state that holds the slot (a cycle that
    // lives as long as the app); fine until a test builds one.
    let counted = state.clone();
    state.keepalive().install_authorized(Arc::new(ServiceDriver {
        plugin: plugin.0.clone(),
        live: Box::new(move || counted.live_count()),
    }));
}
