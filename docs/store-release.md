# Store releases: TestFlight and Play internal testing

Internal testing only (you and invited testers); public release is later.

**Build number.** Every upload carries the git commit count of the build
(`git rev-list --count HEAD`) as iOS `CFBundleVersion` and Android
`versionCode`; the marketing version is `tauri.conf.json` `version` (0.1.0).
Stores reject a number they have seen, so build store releases from `main`
after a commit (the scripts refuse uncommitted changes and warn off `main`).
To re-upload the same commit after a rejected upload, set a higher number:
`TETHER_BUILD_NUMBER=<n>`.

## iOS: TestFlight

### Once

Done already (2026-11-11): explicit App ID `ca.clym.termlyx`, app record
"Termlyx" in App Store Connect (SKU `ca.clym.termlyx`).

1. Xcode → Settings… → Accounts: your Apple ID is listed with your team
   (its Team ID is in `~/.config/termlyx/apple-team-id`; the upload uses it; no API key).
2. After the first upload (below) has processed, in
   [App Store Connect](https://appstoreconnect.apple.com) → Apps → Termlyx →
   **TestFlight** tab:
   1. Left column, **Internal Testing** → **+** (Create Group) → name it
      `Team` → **Create**.
   2. In the group: **Testers** → **+** → tick the people (they must be users
      in Users and Access, up to 100) → **Add**. Tick **Enable automatic
      distribution** so new builds reach them without a click.
3. **Export compliance** (done 2026-11-11): App Store Connect → Termlyx →
   App Information → app encryption documentation, answered once: standard
   encryption algorithms, nothing proprietary, not distributed in France. It
   needs no documentation, so the app declares
   `ITSAppUsesNonExemptEncryption = NO` and builds go straight to TestFlight.
   (YES would make every upload fail with "Invalid Export Compliance Code"
   unless Apple issued a code.) Before a public release that includes
   France, add the French declaration there. Not legal advice.
4. Testers install the **TestFlight** app from the App Store and accept the
   email invitation (or open the TestFlight app signed in with the invited
   Apple ID).

### Each release (Mac)

```sh
git status                        # clean, on main
export CARGO_TARGET_DIR=$HOME/tether-target
scripts/ios.sh release --upload
```

It builds the release archive, exports it for App Store Connect (Xcode signs
it with a cloud-managed Apple Distribution certificate), checks the IPA
(build number, encryption key, distribution signature), keeps a copy in
`target/ios/Termlyx-<version>-<build>.ipa`, then uploads. The build shows
under TestFlight → iOS Builds after processing (minutes to ~30); with
automatic distribution on, testers get it then.

Without `--upload` it stops after the IPA. To upload that file by hand: open
**Transporter** (free, Mac App Store), sign in, drag the `.ipa` in →
**Deliver**.

If the export fails with an account message: Xcode → Settings… → Accounts →
select your Apple ID → if it says the session expired, sign in again; then
rerun.

The TestFlight build and a dev build from `scripts/ios.sh device-run` are the
same app id: installing one replaces the other on the device; the vault stays.

## Android: Play internal testing

The Play Console developer account exists. Android work starts once iOS
reaches TestFlight.

### Once

1. **Upload key** (win-dev, over RDP, PowerShell). Skip if
   `Test-Path $env:USERPROFILE\.tether\android-release.jks` says True. It asks
   for a password twice and your name/org; type them there only:

   ```powershell
   mkdir $env:USERPROFILE\.tether
   & "$env:LOCALAPPDATA\Android\jdk17\bin\keytool.exe" -genkeypair -v -keystore $env:USERPROFILE\.tether\android-release.jks -alias tether -keyalg RSA -keysize 4096 -validity 10000
   ```

   Back up `android-release.jks` and its password (password manager). It is
   the Play **upload key**; Google holds the app signing key. A lost upload
   key can be reset through Play Console support.
2. [Play Console](https://play.google.com/console) → **Create app**: App name
   `Termlyx`, Default language English, **App**, **Free**, tick the two
   declarations → **Create app**. The package name `ca.clym.termlyx` is set
   by the first upload and can never change.
3. Left menu **Test and release → Testing → Internal testing** → **Testers**
   tab → **Create email list** → name `Team`, add the testers' Google account
   emails → **Save changes** → tick the list → **Save**. Copy the **Join on
   the web** link for testers.
4. First release (after building and signing below): **Internal testing** →
   **Create new release** → Play App Signing prompt: **Use Google-generated
   key** (default) → drag `target/android/termlyx-play.aab` into **App
   bundles** → Release name auto-fills (`<versionCode> (0.1.0)`) → **Next** →
   fix any errors it lists → **Save and publish**.
5. Play asks for **App content** (left menu **Policy and programs → App
   content**) before or right after the first release. Expected for Termlyx:
   - Privacy policy: a URL is needed (the app asks for camera). A short page
     on your own site: no data collected; everything stays on device or on
     your own sync server.
   - App access: all functionality available without special access (or
     note that a self-hosted server is optional).
   - Ads: no. Content rating: questionnaire, utility app. Target audience:
     18+. Data safety: no data collected or shared.
   - Foreground service (`specialUse`, "Keep sessions alive"): describe it as
     keeping user-started SSH sessions connected while the app is in the
     background; a short screen recording may be asked for.
6. Testers open the **Join on the web** link with their Google account,
   accept, then install from the Play Store link shown. On the tablet:
   **uninstall any non-Play build first** (dev build or a sideloaded release
   APK: different signature). That deletes its
   local vault: sync it or export first.

A new personal Play account must run a closed test (12 testers, 14 days)
before production; internal testing is not blocked by it.

### Each release

1. Mac, clean tree on `main`:

   ```sh
   scripts/win.sh sync            # only when android-sign.ps1 changed
   scripts/android.sh build play
   ```

   devvm builds the arm64-v8a release AAB with versionCode = build number
   and copies it to win-dev as `~\termlyx-play-unsigned.aab` (an older signed
   `~\termlyx-play.aab` is deleted).
2. win-dev over RDP, PowerShell:

   ```powershell
   powershell -NoProfile -ExecutionPolicy Bypass -File C:\src\termlyx\scripts\win\android-sign.ps1 -Aab
   ```

   `jarsigner` asks for the keystore password, signs, verifies and prints
   the certificate SHA-256 (same every release). Writes `~\termlyx-play.aab`.
3. Mac: `scripts/android.sh aab` → `target/android/termlyx-play.aab`.
4. Play Console → Termlyx → **Internal testing** → **Create new release** →
   drag the AAB in → **Next** → **Save and publish**. Testers get the update
   from the Play Store.
