# Add project specific ProGuard rules here.
# You can control the set of applied configuration files using the
# proguardFiles setting in build.gradle.
#
# For more details, see
#   http://developer.android.com/guide/developing/tools/proguard.html

# If your project uses WebView with JS, uncomment the following
# and specify the fully qualified class name to the JavaScript interface
# class:
#-keepclassmembers class fqcn.of.javascript.interface.for.webview {
#   public *;
#}

# Uncomment this to preserve the line number information for
# debugging stack traces.
#-keepattributes SourceFile,LineNumberTable

# If you keep the line number information, uncomment this to
# hide the original source file name.
#-renamesourcefileattribute SourceFile

# KeepAliveService.stop is only called from Rust over JNI (android.rs), so R8
# sees no caller and would strip it from release builds.
-keep class ca.clym.termlyx.KeepAliveService {
    public static void stop(android.content.Context, long);
}

# The QR scanner (barcode-scanner plugin) uses ML Kit from Google Play services. R8 broke its
# internals in release builds: scanning crashed with an NPE in mlkit_vision_common (build 401).
-keep class com.google.mlkit.** { *; }
-keep class com.google.android.gms.internal.mlkit_** { *; }
