# Signs the unsigned release APK that `scripts/android.sh build release` put on
# this VM. The owner runs it in PowerShell over RDP (it needs a console to ask
# for the password). apksigner asks for the keystore password itself: this
# script has no password parameter and never reads, stores or passes one.
#
#   .\android-sign.ps1                         # ~\termlyx-release-unsigned.apk -> ~\termlyx-release.apk
#   .\android-sign.ps1 -Keystore D:\k\x.jks    # other keystore (alias still `tether` unless -Alias)
#   .\android-sign.ps1 -Aab                    # Play upload: ~\termlyx-play-unsigned.aab -> ~\termlyx-play.aab
#                                              # (jarsigner; same keystore = the Play upload key)
param(
  [switch]$Aab,
  [string]$In = $(if ($Aab) { "$env:USERPROFILE\termlyx-play-unsigned.aab" } else { "$env:USERPROFILE\termlyx-release-unsigned.apk" }),
  [string]$Out = $(if ($Aab) { "$env:USERPROFILE\termlyx-play.aab" } else { "$env:USERPROFILE\termlyx-release.apk" }),
  [string]$Keystore = "$env:USERPROFILE\.tether\android-release.jks",
  [string]$Alias = 'tether'
)
$ErrorActionPreference = 'Stop'

# JDK (for apksigner.bat, and jarsigner for an AAB): JAVA_HOME, else java on PATH, else the VM's Android\jdk17. Set for this process only.
$jdk = @($env:JAVA_HOME, "$env:LOCALAPPDATA\Android\jdk17") | Where-Object { $_ -and (Test-Path "$_\bin\java.exe") } | Select-Object -First 1
if ($jdk) { $env:JAVA_HOME = $jdk }
elseif (-not (Get-Command java -ErrorAction SilentlyContinue)) {
  throw "no JDK: JAVA_HOME='$env:JAVA_HOME' has no bin\java.exe, java is not on PATH, and $env:LOCALAPPDATA\Android\jdk17 is missing. See scripts/win/README.md (Android release signing)"
}
if (-not (Test-Path $In)) { throw "missing $In; run scripts/android.sh build $(if ($Aab) { 'play' } else { 'release' }) on the Mac first" }
if (-not (Test-Path $Keystore)) { throw "missing keystore $Keystore; create it once with the keytool command in scripts/win/README.md" }

if ($Aab) {
  # An AAB is signed with jarsigner (no zipalign; Play re-signs installs with the app signing key).
  # JDK tools by full path when $jdk is known, else from PATH (with java).
  $bin = if ($jdk) { "$jdk\bin\" } else { '' }
  $jarsigner = "${bin}jarsigner"
  Remove-Item $Out -ErrorAction SilentlyContinue
  # No -storepass: jarsigner prompts for the password on the console.
  & $jarsigner -keystore $Keystore -sigalg SHA256withRSA -digestalg SHA-256 -signedjar $Out $In $Alias
  if ($LASTEXITCODE) { throw "jarsigner failed ($LASTEXITCODE)" }
  & $jarsigner -verify -certs $Out | Select-Object -Last 3
  if ($LASTEXITCODE) { throw "jarsigner -verify failed ($LASTEXITCODE)" }
  & "${bin}keytool" -printcert -jarfile $Out | Select-String 'SHA256:'
  Write-Host "signed: $Out ($([math]::Round((Get-Item $Out).Length / 1MB, 1)) MB). On the Mac: scripts/android.sh aab"
  exit 0
}

# SDK root: first of these that has build-tools.
$roots = @($env:ANDROID_HOME, $env:ANDROID_SDK_ROOT, "$env:LOCALAPPDATA\Android\Sdk", "$env:LOCALAPPDATA\Android") |
  Where-Object { $_ -and (Test-Path "$_\build-tools") }
# Newest build-tools dir that has both tools.
$bt = $roots | ForEach-Object { Get-ChildItem "$_\build-tools" -Directory } |
  Where-Object { (Test-Path "$($_.FullName)\zipalign.exe") -and (Test-Path "$($_.FullName)\apksigner.bat") } |
  Sort-Object { try { [version]$_.Name } catch { [version]'0.0' } } | Select-Object -Last 1
if (-not $bt) {
  throw ("no build-tools with zipalign and apksigner. Searched: ANDROID_HOME='$env:ANDROID_HOME', ANDROID_SDK_ROOT='$env:ANDROID_SDK_ROOT', " +
    "$env:LOCALAPPDATA\Android\Sdk, $env:LOCALAPPDATA\Android (each needs a build-tools folder). See scripts/win/README.md (Android release signing)")
}

$aligned = "$Out.aligned"
Remove-Item $aligned, $Out -ErrorAction SilentlyContinue
& "$($bt.FullName)\zipalign.exe" -p 4 $In $aligned
if ($LASTEXITCODE) { throw "zipalign failed ($LASTEXITCODE)" }
try {
  # No --ks-pass: apksigner prompts for the password on the console.
  & "$($bt.FullName)\apksigner.bat" sign --ks $Keystore --ks-key-alias $Alias --out $Out $aligned
  if ($LASTEXITCODE) { throw "apksigner sign failed ($LASTEXITCODE)" }
} finally {
  Remove-Item $aligned -ErrorAction SilentlyContinue
}
& "$($bt.FullName)\apksigner.bat" verify --print-certs $Out
if ($LASTEXITCODE) { throw "apksigner verify failed ($LASTEXITCODE)" }
Write-Host "signed: $Out ($([math]::Round((Get-Item $Out).Length / 1MB, 1)) MB). On the Mac: scripts/android.sh apk"
