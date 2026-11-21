# Windows dev loop (Mac -> `win-dev` VM)

```sh
git commit ...                       # sync sends committed HEAD only (WIP commits are fine)
scripts/win.sh sync                  # bundle HEAD, scp it, check it out in C:\src\termlyx
scripts/win.sh run tsc vitest build  # run steps on the VM; prints "step exit=N secs=S"
```

Steps: `cargo` `clippy` `guard` `tsc` `vitest` `playwright` `build` (debug,
`--no-bundle`) `bundle` (release, NSIS installer) `fixture` (SSH-fixture
tests, below). `run` exits non-zero if any step failed and copies each step's
log to `$CARGO_TARGET_DIR/win-logs/<step>.log` (default `target/win-logs/`).
JS steps first run `pnpm install --frozen-lockfile` on the VM
(`pnpm-install.log`).

Env: `TETHER_WIN_HOST` (required, `user@host`), `TETHER_WIN_KEY`
(default `~/.ssh/id_ed25519`).

The VM keeps its own `C:\src\termlyx\target` and `node_modules` (`sync` never
deletes ignored files). Do not point `CARGO_TARGET_DIR` on the VM at a Mac path.
`sync` sets `core.autocrlf false` in the VM clone (idempotent) so the checkout
is LF, byte-identical to the Mac; when it flips the setting it rewrites the
working tree once (`git rm --cached -r . && git reset --hard`).
The VM's default SSH shell is cmd; `steps.ps1` is Windows PowerShell 5.1 and
runs each step through `cmd /c` (PowerShell script policy blocks `pnpm.ps1`).

`steps.ps1` sets `CARGO_INCREMENTAL=0`: incremental caches grew with every
synced commit and once filled the VM's disk (`target\debug\incremental` and
`target\release\incremental` are safe to delete if they come back).

## SSH-fixture tests (`fixture`)

`scripts/win.sh run fixture` runs `cargo test --workspace --no-fail-fast` on
the VM with `TETHER_SSH_FIXTURE=1` against the Docker fixture the **Mac**
starts (`tests/fixtures/`); the VM needs no Docker or WSL. The tests stay
unchanged. The Mac's Docker is devvm's by default: `win.sh` opens the
`scripts/devvm.sh` tunnel first (and closes it after, if it opened it), so
the Mac's `127.0.0.1:2222` and its `docker` CLI reach devvm
(`tests/fixtures/README.md`). `TETHER_DOCKER=local` uses Docker Desktop
instead.

1. Mac: the devvm tunnel (unless `TETHER_DOCKER=local`),
   `tests/fixtures/up.sh`, then `docker-relay.py` on a Unix socket in a
   private temp dir.
2. A separate SSH session for `fixture` alone, with `ExitOnForwardFailure=yes`
   and two reverse forwards, both bound to the VM's `127.0.0.1` only:
   `-R 127.0.0.1:2222:127.0.0.1:2222` (the fixture's sshd, same address the
   tests use on the Mac) and `-R 127.0.0.1:22375:<relay socket>`.
3. VM: `steps.ps1` builds `docker-shim.cs` into `C:\src\fixture-bin\docker.exe`
   (Add-Type), puts it first on PATH for this step only, checks
   `docker exec tether-sshd true`, then runs the tests.
4. Mac, when that session ends, and from the trap on any exit of `win.sh`:
   stop the relay, `tests/fixtures/down.sh`, close the devvm tunnel if
   `win.sh` opened it.

The tests shell out to `docker` (`docker exec tether-sshd ...` for setup and
checks, `docker compose ... restart sshd` for the reconnect test). The shim
sends its argv over the forward; the relay runs only those two shapes against
the Mac's Docker CLI, i.e. devvm's daemon through the tunnel (the compose
path is swapped for the Mac's copy) and refuses
anything else (exit 125). The fixture's test credentials and the relay never
leave loopback: the forwards exist only inside the SSH session.

What this exposes: while the fixture step runs, any process on the VM's
loopback can run root commands inside the throwaway `tether-sshd` container,
which has outbound network (on devvm: the LAN; with `TETHER_DOCKER=local`
also `host.docker.internal`, i.e. services on the Mac's loopback); acceptable
for this single-user dev VM.

When `fixture` is in a `run` with other steps, they run first in a plain
session; the fixture, relay and forwards exist only for the `fixture` session
that follows. Notes:

- `down.sh` stops the Mac fixture at the end, also when it was already up;
  don't overlap with a Mac `acceptance.sh` run.
- A run killed with SIGKILL leaves the fixture up (`tests/fixtures/down.sh`);
  the relay exits by itself when `win.sh` is gone.
- "remote port forwarding failed": something on the VM holds 2222 or 22375
  (an older session still running?).
- `steps.ps1 -Steps fixture` run on the VM without `win.sh` fails fast with
  "no relay to the Mac fixture".

## No Credential Manager over SSH

A key-authenticated SSH session has no cached logon credentials and no
interactive desktop: Credential Manager, DPAPI-backed secrets and Windows Hello
do not work in it. Those checks are not part of this loop; run them by hand
from an RDP or console session on the VM.

Run 2026-10-23 (RDP session, elevated Windows Terminal, from
`C:\src\termlyx`):

```powershell
cargo test -p core-api -p core-sync --test windows_interactive -- --ignored --nocapture
```

Result: 3 passed, `equal: true`; the Windows Hello PIN dialog came up in front
of the window. Hello works over RDP; no console session is needed. Both crates
are needed (the sync-token test lives in core-sync). The terminal must be
elevated: the `target` files were created by the (elevated) SSH sessions.

## VM setup (done 2026-10-23)

The Windows 11 dev VM (any host name; an admin user). To redo, in an elevated
PowerShell.

SSH side (the loop rests on this). The default SSH shell is left at cmd (no
`HKLM:\SOFTWARE\OpenSSH\DefaultShell` value). Put the Mac's public key
(`~/.ssh/id_ed25519.pub`) in the file below; for an admin user
sshd reads this file instead of `~\.ssh\authorized_keys`, and it must be
writable only by SYSTEM and Administrators:

```powershell
Add-WindowsCapability -Online -Name OpenSSH.Server~~~~0.0.1.0
Set-Service sshd -StartupType Automatic     # the VM was left at Manual on 2026-10-23
Start-Service sshd
Add-Content C:\ProgramData\ssh\administrators_authorized_keys '<contents of the .pub file>'
icacls.exe C:\ProgramData\ssh\administrators_authorized_keys /inheritance:r /grant "SYSTEM:(F)" /grant "BUILTIN\Administrators:(F)"
```

Toolchain:

```powershell
winget install --id Git.Git -e
winget install --id Rustlang.Rustup -e              # then: rustup default stable
winget install --id OpenJS.NodeJS.LTS -e            # Node 24 (includes corepack)
winget install --id Microsoft.VisualStudio.2022.BuildTools -e --override "--passive --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
corepack enable
corepack install -g pnpm@12.4.1                     # matches packageManager in apps/tauri/package.json
```

WebView2 ships with Windows 11. Clone the repo once from a bundle (there is no
remote): on the Mac `git bundle create /tmp/t.bundle --all`, `scp` it to the VM,
then:

```powershell
git clone -c core.autocrlf=false C:\src\t.bundle C:\src\termlyx   # LF checkout, like the Mac (fresh Git for Windows defaults to autocrlf true)
git -C C:\src\termlyx config core.autocrlf false                    # sync also sets this, but only renormalizes when it flips the setting
```

`sync` takes over from there. One-time Playwright browser (needed by the
`playwright` step), after `pnpm -C apps/tauri install` has run in the clone:

```powershell
cd C:\src\termlyx
cmd /c "pnpm -C apps/tauri exec playwright install chromium"
```

New SSH sessions do not see PATH changes made by installers; `steps.ps1`
rebuilds `$env:Path` from the Machine and User registry values each run. Run
pnpm as `pnpm -C apps/tauri ...` (from the repo root corepack fetches the
wrong pnpm).

## Android release signing (owner only, over RDP)

`scripts/android.sh build release` builds an **unsigned** arm64-v8a APK on
devvm and leaves it on this VM as `~\termlyx-release-unsigned.apk`. You sign it
here with `android-sign.ps1`. `apksigner` asks for the keystore password
itself; nothing in the repo, the scripts, SSH or the builder ever sees it.

One-time setup, checked on the VM 2026-10-31. The JDK is there already:
Temurin 17 in `%LOCALAPPDATA%\Android\jdk17` (not on PATH, `JAVA_HOME`
unset; the script finds it and sets `JAVA_HOME` for its own process only).
The SDK is `%LOCALAPPDATA%\Android\Sdk` (no `ANDROID_HOME`), but it has no
`build-tools` (it only has cmdline-tools, emulator, platform-tools,
system-images), so `zipalign` and `apksigner` are missing. In PowerShell over
RDP, once (skip it if `dir $env:LOCALAPPDATA\Android\Sdk\build-tools` already
lists a version):

```powershell
$env:JAVA_HOME = "$env:LOCALAPPDATA\Android\jdk17"   # sdkmanager needs java
& "$env:LOCALAPPDATA\Android\Sdk\cmdline-tools\latest\bin\sdkmanager.bat" "build-tools;36.0.0"
```

(devvm builds with build-tools 36.0.0; any version works, the script takes the
newest.) If the VM is ever rebuilt without that JDK: `winget install --id EclipseAdoptium.Temurin.17.JDK -e`.

One-time keystore, outside the repo. `keytool` is not on PATH, so use its full
path; it asks for the password twice (and your name, org, ...), type it there
and nowhere else:

```powershell
mkdir $env:USERPROFILE\.tether
& "$env:LOCALAPPDATA\Android\jdk17\bin\keytool.exe" -genkeypair -v -keystore $env:USERPROFILE\.tether\android-release.jks -alias tether -keyalg RSA -keysize 4096 -validity 10000
```

**Back `android-release.jks` up (and the password, in your password manager).**
Updates must be signed with the same key: lose it and the phone has to
uninstall Termlyx (losing its local vault) before a new key installs.

Play Store builds (AAB, same keystore as the upload key): `android-sign.ps1 -Aab`; steps in `docs/store-release.md`.

Each release (sideload APK):

1. Mac: `scripts/win.sh sync` (puts `android-sign.ps1` on the VM, once per change to it), then `scripts/android.sh build release`.
2. VM, over RDP, in PowerShell (`-ExecutionPolicy Bypass` applies to this one process only):
   `powershell -NoProfile -ExecutionPolicy Bypass -File C:\src\termlyx\scripts\win\android-sign.ps1`.
   It picks the newest build-tools, runs `zipalign -p 4`, `apksigner sign`
   (password prompt), `apksigner verify --print-certs`, and writes
   `~\termlyx-release.apk`. Compare the certificate SHA-256 with the one from
   your first release.
3. Mac: `scripts/android.sh apk` copies it to `target/android/termlyx-release.apk` and prints its timestamp. `build release` deletes the previous `~\termlyx-release.apk`, so a forgotten step 2 makes `apk` fail instead of handing over an old build.

## Android emulator AVDs (owner, over RDP)

The emulator and its AVDs live on this VM (`%LOCALAPPDATA%\Android\Sdk`, JDK
`%LOCALAPPDATA%\Android\jdk17`, neither on PATH; `JAVA_HOME` is set per
process). `pixel9` (Pixel 9, API 36, Google APIs x86_64) is the phone AVD
(`tether-phone` in the Android spec) and already exists. Two more, for
the manual Android checks: `tether-tablet` (Pixel Tablet, API 36) and
`tether-min` (small phone, API 30, the oldest Android Termlyx supports). In
PowerShell, once:

```powershell
$env:JAVA_HOME = "$env:LOCALAPPDATA\Android\jdk17"
$bin = "$env:LOCALAPPDATA\Android\Sdk\cmdline-tools\latest\bin"
& "$bin\sdkmanager.bat" "system-images;android-30;google_apis;x86_64"   # API 36 is there for pixel9; this one is new (~1.5 GB, accept the licence with y)
& "$bin\avdmanager.bat" list device | Select-String "pixel_tablet|pixel_2 "   # both ids must show
'no' | & "$bin\avdmanager.bat" create avd -n tether-tablet -k "system-images;android-36;google_apis;x86_64" -d pixel_tablet
'no' | & "$bin\avdmanager.bat" create avd -n tether-min -k "system-images;android-30;google_apis;x86_64" -d pixel_2
# The tablet rows use the PC keyboard: no on-screen keyboard by default.
$ini = "$env:USERPROFILE\.android\avd\tether-tablet.avd\config.ini"
(Get-Content $ini) -replace '^hw.keyboard=.*', 'hw.keyboard=yes' | Set-Content $ini
```

(`'no' |` answers avdmanager's "custom hardware profile" question. If
`list device` lacks an id, run it without the filter and pick the closest:
any 10-inch-class tablet for `tether-tablet`, any 5-inch phone for
`tether-min`.) Start one AVD at a time (RAM); close the emulator window
before starting another. `~\tether-emulator.ps1` only starts `pixel9`. For the
others, use its lines with the AVD name changed:

```powershell
$sdk = "$env:LOCALAPPDATA\Android\Sdk"; $adb = "$sdk\platform-tools\adb.exe"
Start-Process "$sdk\emulator\emulator.exe" -ArgumentList '-avd tether-tablet -no-boot-anim -no-snapshot-save'   # or tether-min
& $adb wait-for-device
while ((& $adb shell getprop sys.boot_completed) -notmatch '1') { Start-Sleep 2 }
& $adb install -r "$env:USERPROFILE\tether.alpha.apk"
& $adb shell am start -n ca.clym.termlyx/.MainActivity
```

Delete `tether-min` when its rows are done (frees ~10 GB):
`& "$bin\avdmanager.bat" delete avd -n tether-min`, and
`& "$bin\sdkmanager.bat" --uninstall "system-images;android-30;google_apis;x86_64"`.
