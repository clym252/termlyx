# Tether Windows dev-loop steps (VM side). Run by scripts/win.sh over SSH; see
# scripts/win/README.md. Windows PowerShell 5.1 compatible.
#
# Usage: steps.ps1 -Steps cargo,tsc,vitest      (comma-separated)
# Steps: cargo clippy guard tsc vitest playwright build bundle fixture
# (fixture needs the reverse tunnel that `scripts/win.sh run fixture` opens.)
#
# Each step's output (stdout + stderr) streams to the console and to
# C:\src\logs\<step>.log. One "<step> exit=N secs=S" line is printed per step.
# Every requested step runs; the script exits 1 if any of them failed.
param(
    [Parameter(Mandatory = $true)][string]$Steps
)

$ErrorActionPreference = 'Continue'

# New SSH sessions do not see PATH changes made by installers; rebuild it from
# the registry (Machine, then User).
$machinePath = [Environment]::GetEnvironmentVariable('Path', 'Machine')
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$env:Path = "$machinePath;$userPath"

$env:CARGO_TERM_COLOR = 'never'
# Incremental caches grow with every synced commit and once filled the VM's
# disk; clean builds of the workspace crates are cheap next to that.
$env:CARGO_INCREMENTAL = '0'
$env:NO_COLOR = '1'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding $false

$repo = 'C:\src\termlyx'
$logs = 'C:\src\logs'
$gitBash = 'C:\Program Files\Git\bin\bash.exe'
$fixtureBin = 'C:\src\fixture-bin'
New-Item -ItemType Directory -Force -Path $logs | Out-Null
Set-Location $repo

# Same guard as the "Guard against test-util in the app" step of
# .github/workflows/ci.yml, run through Git Bash.
$guardScript = @'
set -euo pipefail
tree=$(cargo tree -p tether-app -e normal,features)
if grep -F 'core-ssh feature "test-util"' <<<"$tree"; then
  echo "core-ssh/test-util must not be enabled in tether-app's normal dependencies" >&2
  exit 1
fi
if grep -F 'core-api feature "test-util"' <<<"$tree"; then
  echo "core-api/test-util must not be enabled in tether-app's normal dependencies" >&2
  exit 1
fi
'@

# step name -> command line run with cmd.exe (stderr merged into stdout there,
# so PowerShell 5.1 does not wrap native stderr lines in error records).
$commands = @{
    cargo      = 'cargo test --workspace --no-fail-fast'
    clippy     = 'cargo clippy --workspace --all-targets -- -D warnings'
    tsc        = 'pnpm -C apps/tauri exec tsc'
    vitest     = 'pnpm -C apps/tauri vitest run'
    playwright = 'pnpm -C apps/tauri playwright test'
    build      = 'pnpm -C apps/tauri tauri build --debug --no-bundle'
    bundle     = 'pnpm -C apps/tauri tauri build'
    fixture    = 'cargo test --workspace --no-fail-fast'
}
$jsSteps = @('tsc', 'vitest', 'playwright', 'build', 'bundle')

# Runs a command line through cmd.exe, tee-ing combined output to a log file.
# Returns the exit code.
function Invoke-Logged([string]$cmdline, [string]$logPath) {
    $writer = New-Object System.IO.StreamWriter($logPath, $false, (New-Object System.Text.UTF8Encoding $false))
    $writer.AutoFlush = $true
    try {
        & cmd.exe /d /c "$cmdline 2>&1" | ForEach-Object {
            $writer.WriteLine($_)
            Write-Host $_
        }
        return $LASTEXITCODE
    }
    finally {
        $writer.Dispose()
    }
}

function Invoke-Guard([string]$logPath) {
    # Git checks this file out with CRLF when core.autocrlf is on; bash needs LF.
    $tmp = Join-Path $env:TEMP 'tether-guard.sh'
    [IO.File]::WriteAllText($tmp, ($guardScript -replace "`r`n", "`n"), (New-Object System.Text.UTF8Encoding $false))
    $bashTmp = $tmp -replace '\\', '/'
    return (Invoke-Logged "`"$gitBash`" `"$bashTmp`"" $logPath)
}

# fixture: the tests reach the Mac's Docker fixture at 127.0.0.1:2222 and run
# `docker exec tether-sshd ...` / `docker compose ... restart sshd`; both go
# through win.sh's reverse forwards. `docker` here is docker-shim.cs, built
# into $fixtureBin and put first on PATH for this step only.
function Invoke-Fixture([string]$logPath) {
    $exe = Join-Path $fixtureBin 'docker.exe'
    try {
        New-Item -ItemType Directory -Force -Path $fixtureBin | Out-Null
        Remove-Item -Force -ErrorAction SilentlyContinue $exe
        $src = [IO.File]::ReadAllText((Join-Path $repo 'scripts\win\docker-shim.cs'))
        Add-Type -TypeDefinition $src -Language CSharp -OutputAssembly $exe -OutputType ConsoleApplication -ErrorAction Stop
    }
    catch {
        Set-Content -Path $logPath -Value "fixture: could not build the docker shim: $_"
        Write-Host "fixture: could not build the docker shim: $_"
        return 1
    }
    $savedPath = $env:Path
    $env:Path = "$fixtureBin;$savedPath"
    $env:TETHER_SSH_FIXTURE = '1'
    try {
        # Fail fast, with a clear message, when the tunnel is not there.
        $check = Invoke-Logged 'docker exec tether-sshd true' $logPath
        if ($check -ne 0) {
            $msg = 'fixture: no relay to the Mac fixture (run this step through scripts/win.sh run fixture)'
            Add-Content -Path $logPath -Value $msg
            Write-Host $msg
            return 1
        }
        return (Invoke-Logged $commands['fixture'] $logPath)
    }
    finally {
        $env:Path = $savedPath
        Remove-Item Env:\TETHER_SSH_FIXTURE -ErrorAction SilentlyContinue
    }
}

$requested = @($Steps -split '[,\s]+' | Where-Object { $_ })
foreach ($s in $requested) {
    if (-not ($commands.ContainsKey($s) -or $s -eq 'guard')) {
        Write-Host "unknown step: $s (valid: cargo clippy guard tsc vitest playwright build bundle fixture)"
        exit 2
    }
}

# Drop old logs of the steps about to run, so a log on disk is always from this run.
foreach ($s in $requested + 'pnpm-install') {
    Remove-Item -Force -ErrorAction SilentlyContinue (Join-Path $logs "$s.log")
}

$head = (& git rev-parse HEAD).Trim()
Write-Host "head=$head steps=$($requested -join ',')"

$failed = $false

# JS steps need node_modules in sync with the lockfile; a no-op when it is.
if ($requested | Where-Object { $jsSteps -contains $_ }) {
    $sw = [Diagnostics.Stopwatch]::StartNew()
    $code = Invoke-Logged 'pnpm -C apps/tauri install --frozen-lockfile' (Join-Path $logs 'pnpm-install.log')
    Write-Host ("pnpm-install exit={0} secs={1}" -f $code, [int]$sw.Elapsed.TotalSeconds)
    if ($code -ne 0) { $failed = $true }
}

foreach ($step in $requested) {
    $log = Join-Path $logs "$step.log"
    $sw = [Diagnostics.Stopwatch]::StartNew()
    if ($step -eq 'guard') {
        $code = Invoke-Guard $log
    }
    elseif ($step -eq 'fixture') {
        $code = Invoke-Fixture $log
    }
    else {
        $code = Invoke-Logged $commands[$step] $log
        if ($step -eq 'bundle' -and $code -eq 0) {
            # Name the installer(s) in the log (the VM uses the repo's own target\).
            $setups = @(Get-ChildItem -ErrorAction SilentlyContinue (Join-Path $repo 'target\release\bundle\nsis\*.exe'))
            $lines = @($setups | ForEach-Object { 'installer: {0} ({1} bytes)' -f $_.FullName, $_.Length })
            if ($setups.Count -eq 0) { $code = 1; $lines = @('bundle: no installer in target\release\bundle\nsis') }
            Add-Content -Path $log -Value $lines
            $lines | ForEach-Object { Write-Host $_ }
        }
    }
    Write-Host ("{0} exit={1} secs={2}" -f $step, $code, [int]$sw.Elapsed.TotalSeconds)
    if ($code -ne 0) { $failed = $true }
}

if ($failed) { exit 1 }
exit 0
