package ca.clym.termlyx

import android.Manifest
import android.app.Activity
import android.app.ActivityManager
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.provider.DocumentsContract
import android.provider.OpenableColumns
import android.webkit.MimeTypeMap
import android.os.Build
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyPermanentlyInvalidatedException
import android.security.keystore.KeyProperties
import android.security.keystore.StrongBoxUnavailableException
import android.util.Base64
import androidx.activity.result.ActivityResult
import androidx.appcompat.app.AppCompatActivity
import androidx.biometric.BiometricManager
import androidx.biometric.BiometricManager.Authenticators.BIOMETRIC_STRONG
import androidx.biometric.BiometricPrompt
import androidx.core.content.ContextCompat
import androidx.fragment.app.FragmentActivity
import app.tauri.annotation.ActivityCallback
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSArray
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import java.io.File
import java.security.KeyStore
import java.security.UnrecoverableKeyException
import javax.crypto.KeyGenerator
import javax.crypto.Mac
import javax.crypto.SecretKey

/** Quick unlock's Keystore key; one per install, shared by all vaults. */
private const val QUICK_UNLOCK_ALIAS = "dev.tether.client.quick-unlock"
private const val NOT_NOW = "Biometric unlock is not available right now."

@InvokeArg
class BiometricHmacArgs {
  /** Base64; not secret (only the Keystore key turns it into one). */
  var challenge: String = ""
  var reason: String = ""
  var create: Boolean = false
}

@InvokeArg
class KeepAliveArgs {
  /** Open sessions plus running forwards, for the notification's text. */
  var count: Int = 1
  /** The Rust slot's start/stop generation; see `KeepAliveService.latest`. */
  var generation: Long = 0
}

@InvokeArg
class ImportFilesArgs {
  /** A folder inside Tether's folder; Rust has checked it. */
  var dir: String = ""
}

@InvokeArg
class ExportFileArgs {
  /** A file inside Tether's folder; Rust has checked it. */
  var path: String = ""
}

/** A refusal for Rust, by code (`android.rs` maps codes, never messages). The message is a sentence for the user. */
private class Refusal(val code: String, message: String) : Exception(message)

/** Tether's own native commands. Registered by `src-tauri/src/android.rs`; each `@Command` has a Rust caller there. */
@TauriPlugin
class TetherPlugin(activity: Activity) : Plugin(activity) {
  /**
   * The Activity in front now. The plugin is built once per process, but a process kept alive by `KeepAliveService`
   * gets a new Activity when Tether is reopened after a swipe from recents; the one it was built with is destroyed.
   */
  @Volatile
  private var current: Activity = activity

  /** Tauri's `PluginManager.onResume` hook: every Activity, the first one (cold start) and any later one. */
  override fun onResume(activity: AppCompatActivity) {
    current = activity
  }

  /** Back on Hosts with nothing open: to the background, as Home does. Never `finish()`: sessions keep running. */
  @Command
  fun background(invoke: Invoke) {
    current.moveTaskToBack(true)
    invoke.resolve()
  }

  /** Whether the notification permission was asked in this run; it is asked when the first session opens, once. */
  private var askedNotifications = false

  /**
   * Starts the keep-alive foreground service (`KeepAliveService`) when the first session or forward opens. The first
   * call of a run also asks for the notification permission (Android 13+); denial changes nothing, the service runs
   * either way.
   */
  @Command
  fun keepAlive(invoke: Invoke) {
    val args = invoke.parseArgs(KeepAliveArgs::class.java)
    askForNotifications()
    if (startService(args.count, args.generation)) invoke.resolve() else invoke.reject("Keep-alive could not start.", "unavailable")
  }

  private fun startService(count: Int, generation: Long): Boolean =
    try {
      KeepAliveService.start(current.applicationContext, count, generation)
      true
    } catch (e: Exception) {
      // For example Android refusing a start from the background. Rust only logs it; a later count retries while in front.
      false
    }

  /**
   * A new count for the running service's notification (`notify`, never a start, so allowed in the background). If
   * the service is not running (its start was refused, timed out or failed to promote while the slot believes it
   * runs), a count that arrives while Tether is in front starts it; in the background nothing starts (Android
   * refuses it), and the next count in front does. A count from before a later stop (it landed late) does nothing:
   * restarting then would show "N sessions open" with nothing open.
   */
  @Command
  fun keepAliveCount(invoke: Invoke) {
    val args = invoke.parseArgs(KeepAliveArgs::class.java)
    if (args.count > 0 && KeepAliveService.isCurrent(args.generation) &&
      !KeepAliveService.update(current.applicationContext, args.count) && inFront()
    ) {
      startService(args.count, args.generation)
    }
    invoke.resolve()
  }

  private fun inFront(): Boolean {
    val state = ActivityManager.RunningAppProcessInfo()
    ActivityManager.getMyMemoryState(state)
    return state.importance <= ActivityManager.RunningAppProcessInfo.IMPORTANCE_VISIBLE
  }

  /** Stops the service; its notification goes at once. The Rust side stops it directly first; this is its fallback. */
  @Command
  fun keepAliveStop(invoke: Invoke) {
    KeepAliveService.stop(current.applicationContext, invoke.parseArgs(KeepAliveArgs::class.java).generation)
    invoke.resolve()
  }

  private fun askForNotifications() {
    if (askedNotifications || Build.VERSION.SDK_INT < 33) return
    askedNotifications = true
    if (current.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED) return
    val host = current
    host.runOnUiThread { host.requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), 0) }
  }

  /**
   * "Import from device": the system picker (`OPEN_DOCUMENT`, several files, no storage permission), each pick copied
   * into `dir` (a name already there gets " (2)", " (3)"...). Resolves `{ names }`, empty if the picker was cancelled.
   * Rejects with code `io` when a copy fails; copies made before it stay.
   */
  @Command
  fun importFiles(invoke: Invoke) {
    val intent = Intent(Intent.ACTION_OPEN_DOCUMENT)
      .addCategory(Intent.CATEGORY_OPENABLE)
      .setType("*/*")
      .putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true)
    startPicker(invoke, intent, "importPicked")
  }

  @ActivityCallback
  fun importPicked(invoke: Invoke, result: ActivityResult) {
    val data = result.data
    val clip = data?.clipData
    val uris = when {
      result.resultCode != Activity.RESULT_OK || data == null -> emptyList()
      clip != null -> (0 until clip.itemCount).map { clip.getItemAt(it).uri }
      else -> listOfNotNull(data.data)
    }
    if (uris.isEmpty()) return invoke.resolve(JSObject().put("names", JSArray()))
    val dir = File(invoke.parseArgs(ImportFilesArgs::class.java).dir)
    // Off the main thread: a cloud file may take a while to arrive.
    Thread {
      try {
        val names = JSArray()
        for (uri in uris) names.put(copyIn(uri, dir))
        invoke.resolve(JSObject().put("names", names))
      } catch (e: Exception) {
        invoke.reject(e.message ?: "The file could not be copied.", "io")
      }
    }.start()
  }

  /** Copies the document at `uri` into `dir` under a free name and returns that name. A partial copy is removed. */
  private fun copyIn(uri: Uri, dir: File): String {
    val wanted = displayName(uri)
    val input = current.contentResolver.openInputStream(uri) ?: throw Exception("Could not open $wanted.")
    return input.use {
      val (dest, name) = createFree(dir, wanted)
      try {
        dest.outputStream().use { out -> it.copyTo(out) }
      } catch (e: Exception) {
        dest.delete()
        throw Exception("Could not copy $wanted: ${e.message}")
      }
      name
    }
  }

  /** Makes an empty file named `wanted` in `dir`, or the first of `wanted (2)`, `wanted (3)`... that is free. Atomic. */
  private fun createFree(dir: File, wanted: String): Pair<File, String> {
    var n = 1
    while (true) {
      val name = if (n == 1) wanted else numbered(wanted, n)
      val file = File(dir, name)
      if (file.createNewFile()) return file to name
      n++
    }
  }

  /** "photo.jpg" -> "photo (2).jpg"; no extension: "notes" -> "notes (2)". */
  private fun numbered(name: String, n: Int): String {
    val dot = name.lastIndexOf('.')
    return if (dot > 0) "${name.substring(0, dot)} ($n)${name.substring(dot)}" else "$name ($n)"
  }

  /** The picked file's own name, kept to one path segment so it can never name a place outside the folder. */
  private fun displayName(uri: Uri): String {
    val queried = current.contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use {
      if (it.moveToFirst()) it.getString(0) else null
    }
    val name = (queried ?: uri.lastPathSegment ?: "").substringAfterLast('/').trim()
    return if (name.isEmpty() || name == "." || name == "..") "Imported file" else name
  }

  /**
   * "Save to device…": the system "save as" picker (`CREATE_DOCUMENT`, titled with the file's name), then the file at
   * `path` is streamed to the chosen place. Resolves `{ saved }`: false when the picker was cancelled. Rejects with
   * code `io` when the write fails.
   */
  @Command
  fun exportFile(invoke: Invoke) {
    val file = File(invoke.parseArgs(ExportFileArgs::class.java).path)
    val mime = MimeTypeMap.getSingleton().getMimeTypeFromExtension(file.extension.lowercase()) ?: "application/octet-stream"
    val intent = Intent(Intent.ACTION_CREATE_DOCUMENT)
      .addCategory(Intent.CATEGORY_OPENABLE)
      .setType(mime)
      .putExtra(Intent.EXTRA_TITLE, file.name)
    startPicker(invoke, intent, "exportPicked")
  }

  @ActivityCallback
  fun exportPicked(invoke: Invoke, result: ActivityResult) {
    val uri = result.data?.data
    if (result.resultCode != Activity.RESULT_OK || uri == null) return invoke.resolve(JSObject().put("saved", false))
    val file = File(invoke.parseArgs(ExportFileArgs::class.java).path)
    Thread {
      try {
        val out = current.contentResolver.openOutputStream(uri, "wt") ?: throw Exception("Could not open the place chosen.")
        out.use { o -> file.inputStream().use { it.copyTo(o) } }
        invoke.resolve(JSObject().put("saved", true))
      } catch (e: Exception) {
        // The empty document the picker made is no use to anyone.
        try {
          DocumentsContract.deleteDocument(current.contentResolver, uri)
        } catch (ignored: Exception) {
        }
        invoke.reject("Could not save ${file.name}: ${e.message}", "io")
      }
    }.start()
  }

  /** Launches a system picker through Tauri's activity-result launchers (registered when the Activity was created). */
  private fun startPicker(invoke: Invoke, intent: Intent, callback: String) {
    try {
      startActivityForResult(invoke, intent, callback)
    } catch (e: Exception) {
      invoke.reject("The file picker could not be opened.", "io")
    }
  }

  /**
   * Biometric quick unlock (`docs/crypto.md`, "Quick unlock on Android"): HMAC-SHA256 of `challenge` under the
   * Keystore key, after a strong-biometric prompt titled `reason`. Resolves `{ mac }` (base64); rejects with code
   * `cancelled`, `unavailable`, `invalidated` (biometrics enrolled or removed since) or `missing` (no key, `create`
   * false). `create` makes the key, replacing an invalidated one. Never logs the challenge or the MAC.
   */
  @Command
  fun biometricHmac(invoke: Invoke) {
    try {
      val args = invoke.parseArgs(BiometricHmacArgs::class.java)
      val challenge = Base64.decode(args.challenge, Base64.NO_WRAP)
      val mac = keystoreMac(args.create)
      val host = current
      host.runOnUiThread {
        // Never let androidx throw on the main thread (it would end the process) or return without answering: Rust waits.
        try {
          prompt(invoke, host, mac, challenge, args.reason)
        } catch (e: Exception) {
          invoke.reject(NOT_NOW, "unavailable")
        }
      }
    } catch (e: Refusal) {
      invoke.reject(e.message, e.code)
    } catch (e: Exception) {
      invoke.reject(NOT_NOW, "unavailable")
    }
  }

  /** An HMAC on the quick unlock key, waiting for one biometric-authorised use. Invalidation shows here, before any prompt. */
  private fun keystoreMac(create: Boolean): Mac {
    val keyStore = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
    // Android 12+ (keystore2) may already refuse here for an invalidated key, before `Mac.init` could.
    val existing = try {
      keyStore.getKey(QUICK_UNLOCK_ALIAS, null) as SecretKey?
    } catch (e: Exception) {
      if (!isInvalidated(e)) throw e
      if (!create) throw Refusal("invalidated", "Biometrics changed.")
      keyStore.deleteEntry(QUICK_UNLOCK_ALIAS)
      null
    }
    if (existing == null && !create) throw Refusal("missing", "Biometric unlock has no key on this device.")
    // Making a key needs a strong biometric enrolled.
    if (create) requireStrongBiometrics()
    val mac = Mac.getInstance("HmacSHA256")
    try {
      mac.init(existing ?: newKey())
    } catch (e: KeyPermanentlyInvalidatedException) {
      if (!create) throw Refusal("invalidated", "Biometrics changed.")
      keyStore.deleteEntry(QUICK_UNLOCK_ALIAS)
      mac.init(newKey())
    }
    if (!create) requireStrongBiometrics()
    return mac
  }

  private fun isInvalidated(e: Exception) =
    e is UnrecoverableKeyException || e is KeyPermanentlyInvalidatedException ||
      e.cause is KeyPermanentlyInvalidatedException

  /** HMAC-SHA256, every use behind a strong biometric (no PIN, pattern or password), gone when biometrics change. StrongBox when present, else the TEE. */
  private fun newKey(): SecretKey {
    fun spec(strongBox: Boolean) =
      KeyGenParameterSpec.Builder(QUICK_UNLOCK_ALIAS, KeyProperties.PURPOSE_SIGN)
        .setUserAuthenticationRequired(true)
        .setUserAuthenticationParameters(0, KeyProperties.AUTH_BIOMETRIC_STRONG)
        .setInvalidatedByBiometricEnrollment(true)
        .setIsStrongBoxBacked(strongBox)
        .build()
    val generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_HMAC_SHA256, "AndroidKeyStore")
    if (current.packageManager.hasSystemFeature(PackageManager.FEATURE_STRONGBOX_KEYSTORE)) {
      try {
        generator.init(spec(true))
        return generator.generateKey()
      } catch (e: StrongBoxUnavailableException) {
        // Falls through to the TEE.
      }
    }
    generator.init(spec(false))
    return generator.generateKey()
  }

  private fun requireStrongBiometrics() {
    when (BiometricManager.from(current).canAuthenticate(BIOMETRIC_STRONG)) {
      BiometricManager.BIOMETRIC_SUCCESS -> return
      BiometricManager.BIOMETRIC_ERROR_NONE_ENROLLED ->
        throw Refusal("unavailable", "Set up a fingerprint or face in Android Settings to use biometric unlock.")
      BiometricManager.BIOMETRIC_ERROR_NO_HARDWARE ->
        throw Refusal("unavailable", "This device has no fingerprint or face sensor that Android rates as strong.")
      else -> throw Refusal("unavailable", NOT_NOW)
    }
  }

  /** On the UI thread: the system prompt, which unlocks `mac` for one use. */
  private fun prompt(invoke: Invoke, activity: Activity, mac: Mac, challenge: ByteArray, reason: String) {
    // TauriActivity is an AppCompatActivity, so always a FragmentActivity. A gone or stopped one gets no prompt:
    // androidx.biometric returns without any callback once the state is saved, and the invoke would never be answered.
    val host = activity as? FragmentActivity
    if (host == null || host.isFinishing || host.isDestroyed || host.supportFragmentManager.isStateSaved) {
      return invoke.reject(NOT_NOW, "unavailable")
    }
    val callback = object : BiometricPrompt.AuthenticationCallback() {
      override fun onAuthenticationSucceeded(result: BiometricPrompt.AuthenticationResult) {
        val out = try {
          result.cryptoObject?.mac?.doFinal(challenge)
        } catch (e: Exception) {
          null
        } ?: return invoke.reject(NOT_NOW, "unavailable")
        val reply = JSObject()
        reply.put("mac", Base64.encodeToString(out, Base64.NO_WRAP))
        out.fill(0)
        invoke.resolve(reply)
      }

      override fun onAuthenticationError(errorCode: Int, errString: CharSequence) {
        when (errorCode) {
          BiometricPrompt.ERROR_USER_CANCELED,
          BiometricPrompt.ERROR_NEGATIVE_BUTTON,
          BiometricPrompt.ERROR_CANCELED -> invoke.reject("Cancelled.", "cancelled")
          // Too many attempts, sensor busy, no biometrics now: the system's own sentence.
          else -> invoke.reject(errString.toString(), "unavailable")
        }
      }
      // onAuthenticationFailed (a finger not recognised): the prompt stays up and the user tries again.
    }
    val info = BiometricPrompt.PromptInfo.Builder()
      // The core's reasons are mid-sentence ("unlock your Tether vault").
      .setTitle(reason.replaceFirstChar { it.uppercase() })
      .setNegativeButtonText("Use master password")
      .setAllowedAuthenticators(BIOMETRIC_STRONG)
      .build()
    BiometricPrompt(host, ContextCompat.getMainExecutor(activity), callback)
      .authenticate(info, BiometricPrompt.CryptoObject(mac))
  }
}
