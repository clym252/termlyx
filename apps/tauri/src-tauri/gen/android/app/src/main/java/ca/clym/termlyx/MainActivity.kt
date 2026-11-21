package ca.clym.termlyx

import android.graphics.Color
import android.os.Bundle
import android.view.View
import android.webkit.WebView
import androidx.activity.SystemBarStyle
import androidx.activity.enableEdgeToEdge
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat
import androidx.webkit.ScriptHandler
import androidx.webkit.WebViewCompat
import androidx.webkit.WebViewFeature

class MainActivity : TauriActivity() {
  // The latest insets as a document-start script, so a page that loads later (or reloads) starts with them.
  private var insetsScript: ScriptHandler? = null

  override fun onCreate(savedInstanceState: Bundle?) {
    // Also stops the decor fitting the system bars (setDecorFitsSystemWindows(false)): the page clears them itself.
    // dark() = light icons: the app is dark in either system mode, and the default (auto) draws dark icons on a light-mode system.
    enableEdgeToEdge(
      statusBarStyle = SystemBarStyle.dark(Color.TRANSPARENT),
      navigationBarStyle = SystemBarStyle.dark(Color.TRANSPARENT),
    )
    super.onCreate(savedInstanceState)
  }

  /**
   * The page gets the system bars and display cutout as `--inset-*` on `<html>`, and the IME's height as
   * `window.__TETHER_IME_PX__` plus a `"tether-ime"` event, in CSS px. Numbers only. The WebView itself sees no
   * insets and never resizes (`adjustNothing`): the page is the one place they are applied.
   */
  override fun onWebViewCreate(webView: WebView) {
    val content = findViewById<View>(android.R.id.content)
    ViewCompat.setOnApplyWindowInsetsListener(content) { _, insets ->
      val density = resources.displayMetrics.density
      fun css(px: Int) = px / density
      val bars = insets.getInsets(WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout())
      // From the window's bottom, gesture bar included: the app drops its bottom inset while the IME is up.
      val ime = insets.getInsets(WindowInsetsCompat.Type.ime()).bottom
      val js = "(()=>{const s=document.documentElement.style;" +
        "s.setProperty('--inset-top','${css(bars.top)}px');" +
        "s.setProperty('--inset-right','${css(bars.right)}px');" +
        "s.setProperty('--inset-bottom','${css(bars.bottom)}px');" +
        "s.setProperty('--inset-left','${css(bars.left)}px');" +
        "window.__TETHER_IME_PX__=${css(ime)};" +
        "window.dispatchEvent(new Event('tether-ime'))})()"
      webView.evaluateJavascript(js, null)
      // shortcut: without DOCUMENT_START_SCRIPT a page loaded after the last insets change starts without them; every WebView minSdk 30 ships with an up-to-date one has it
      if (WebViewFeature.isFeatureSupported(WebViewFeature.DOCUMENT_START_SCRIPT)) {
        insetsScript?.remove()
        insetsScript = WebViewCompat.addDocumentStartJavaScript(webView, js, setOf("*"))
      }
      WindowInsetsCompat.CONSUMED
    }
    ViewCompat.requestApplyInsets(content)
  }
}
