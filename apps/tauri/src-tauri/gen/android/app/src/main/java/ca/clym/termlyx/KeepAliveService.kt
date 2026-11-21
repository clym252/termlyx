package ca.clym.termlyx

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import android.util.Log
import androidx.core.content.ContextCompat

/**
 * Background keep-alive (docs spec A7): while it runs, Android keeps the app's process, and so the SSH sessions and
 * port forwards in it, running in the background. Started by [TetherPlugin] (`keepAlive`) from the Rust keep-alive
 * slot, which runs it only while the setting is on and a session or forward is open, and stopped by the slot straight
 * through [stop] (`android.rs`, no Activity needed). The notification shows a count and nothing else: no
 * host names, nothing from the vault.
 *
 * It lives in the app's process and must never be the process's entry point (the credential store needs the
 * Activity first): `START_NOT_STICKY`, so Android never restarts it alone. It touches no Rust or keyring code.
 */
class KeepAliveService : Service() {
  override fun onBind(intent: Intent?): IBinder? = null

  /** Promoted at creation too, so a stop that lands before `onStartCommand` cannot leave `startForegroundService` unanswered. */
  override fun onCreate() {
    super.onCreate()
    if (promote(1)) running = true
  }

  /** A start carries the count; later counts arrive through [update], which never starts anything. */
  override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
    promote(intent?.getIntExtra(EXTRA_COUNT, 1)?.coerceAtLeast(1) ?: 1)
    return START_NOT_STICKY
  }

  /**
   * Android 12+ refuses `startForeground` from the background in some states. Uncaught that kills the process and
   * every session in it, so a refusal ends the service instead (no half-started service, no notification).
   */
  private fun promote(count: Int): Boolean {
    return try {
      val notification = build(this, count)
      if (Build.VERSION.SDK_INT >= 34) {
        startForeground(NOTIFICATION_ID, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE)
      } else {
        startForeground(NOTIFICATION_ID, notification)
      }
      true
    } catch (e: Exception) {
      Log.w(TAG, "foreground promotion refused: ${e.javaClass.simpleName}")
      stopSelf()
      false
    }
  }

  /** Stopped (`Context.stopService`): the notification goes with it, at once. */
  override fun onDestroy() {
    running = false
    stopForeground(STOP_FOREGROUND_REMOVE)
    // Also when promotion failed and a count `notify` slipped in: no foreground, so the line above leaves it.
    getSystemService(NotificationManager::class.java).cancel(NOTIFICATION_ID)
    super.onDestroy()
  }

  companion object {
    const val EXTRA_COUNT = "count"
    private const val TAG = "TetherKeepAlive"
    private const val CHANNEL = "open_sessions"
    private const val NOTIFICATION_ID = 1

    /** Whether the service is alive in this process (set on its main thread). */
    @Volatile
    private var running = false

    /**
     * The newest start or stop generation seen (Rust's keep-alive slot numbers them). A start, stop or count with an
     * older one landed late, after a newer call, and is dropped. Read and set under the companion's lock.
     */
    private var latest = 0L

    /**
     * Starts the service with `count` in its notification, unless a newer stop or start was already seen (then
     * nothing). Throws when Android refuses the start.
     */
    @Synchronized
    fun start(context: Context, count: Int, generation: Long) {
      if (generation < latest) return
      latest = generation
      ContextCompat.startForegroundService(
        context,
        Intent(context, KeepAliveService::class.java).putExtra(EXTRA_COUNT, count),
      )
    }

    /** Stops the service (its notification goes at once), unless a newer start was already seen. Called over JNI. */
    @JvmStatic
    @Synchronized
    fun stop(context: Context, generation: Long) {
      if (generation < latest) return
      latest = generation
      context.stopService(Intent(context, KeepAliveService::class.java))
    }

    /** Whether a count of `generation` is current: no stop or start since the start it belongs to. */
    @Synchronized
    fun isCurrent(generation: Long) = generation >= latest

    private fun build(context: Context, count: Int): Notification {
      context.getSystemService(NotificationManager::class.java).createNotificationChannel(
        NotificationChannel(CHANNEL, "Open sessions", NotificationManager.IMPORTANCE_LOW),
      )
      val open = PendingIntent.getActivity(
        context,
        0,
        Intent(context, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP),
        PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
      )
      return Notification.Builder(context, CHANNEL)
        .setSmallIcon(R.drawable.ic_stat_tether)
        .setContentTitle("Termlyx")
        .setContentText(if (count == 1) "1 session open" else "$count sessions open")
        .setContentIntent(open)
        .setOngoing(true)
        .setOnlyAlertOnce(true)
        .build()
    }

    /**
     * A new count for the running service: `notify` only (allowed in the background), nothing is started. Returns
     * false, doing nothing, when the service is not running.
     */
    fun update(context: Context, count: Int): Boolean {
      if (!running) return false
      context.getSystemService(NotificationManager::class.java).notify(NOTIFICATION_ID, build(context, count.coerceAtLeast(1)))
      return true
    }
  }
}
