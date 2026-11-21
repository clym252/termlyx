# Install or update the Termlyx server with Docker Desktop. Re-run to update.
#   irm https://github.com/clym252/termlyx/releases/latest/download/install.ps1 | iex
# Hidden overrides: TERMLYX_RELEASE_BASE (where compose.yml and SHA256SUMS come from),
# TERMLYX_IMAGE (a local or mirrored image; skips the pull).
# TERMLYX_PORT (host port, default 8443) is remembered for later runs.
# Not 'Stop': in Windows PowerShell 5.1 that turns native stderr into exceptions.
# Cmdlets that must fail loudly carry -ErrorAction Stop; native exit codes are checked.
$ErrorActionPreference = 'Continue'

$Owner = 'clym252' # GitHub owner; keep in step with compose.yml and release.yml
$Base = if ($env:TERMLYX_RELEASE_BASE) { $env:TERMLYX_RELEASE_BASE } else { "https://github.com/$Owner/termlyx/releases/latest/download" }
$Dir = Join-Path $HOME 'termlyx-server'
$Help = 'https://docs.docker.com/desktop/setup/install/windows-install/'

# Die throws; the catch at the end prints one red line. No 'exit': under irm | iex it would close the window.
function Die($m) { throw $m }

try {
if (-not (Get-Command docker -ErrorAction SilentlyContinue)) { Die "Docker is not installed. Install Docker Desktop from $Help and run this again." }
docker compose version *> $null
if ($LASTEXITCODE -ne 0) { Die "Docker Compose v2 is missing. See $Help and run this again." }
docker info *> $null
if ($LASTEXITCODE -ne 0) { Die 'Cannot talk to Docker. Start Docker Desktop and run this again.' }

New-Item -ItemType Directory -Force -Path $Dir -ErrorAction Stop | Out-Null
Push-Location $Dir
try {
# Download beside the old file, replace it only on success and a checksum match.
foreach ($f in 'compose.yml', 'SHA256SUMS') {
    try { Invoke-WebRequest -UseBasicParsing -Uri "$Base/$f" -OutFile "$f.tmp" -ErrorAction Stop }
    catch { Remove-Item compose.yml.tmp, SHA256SUMS.tmp -ErrorAction SilentlyContinue; Die "could not download $Base/$f ($($_.Exception.Message))" }
}
# sha256sum writes "<hex>  <name>", or "<hex> *<name>" in binary mode. -ieq: Get-FileHash is uppercase.
$want = $null
foreach ($l in Get-Content SHA256SUMS.tmp) {
    if ($l -match '^([0-9A-Fa-f]{64}) [ *]compose\.yml\s*$') { $want = $Matches[1]; break }
}
$got = (Get-FileHash -Algorithm SHA256 compose.yml.tmp).Hash
Remove-Item SHA256SUMS.tmp -ErrorAction SilentlyContinue
if (-not $want -or $got -ine $want) { Remove-Item compose.yml.tmp -ErrorAction SilentlyContinue; Die 'compose.yml did not match the release checksum' }
Move-Item -Force compose.yml.tmp compose.yml -ErrorAction Stop
# Compose reads this file next to compose.yml. It carries TERMLYX_IMAGE and
# TERMLYX_PORT; the port from the environment wins, else the saved one stays.
$Vars = '.env'
$Port = "$($env:TERMLYX_PORT)"
if (-not $Port -and (Test-Path $Vars)) {
    $m = Select-String -Path $Vars -Pattern '^TERMLYX_PORT=(.*)$' | Select-Object -Last 1
    if ($m) { $Port = $m.Matches[0].Groups[1].Value.Trim() }
}
if ($Port -and (($Port -notmatch '^[1-9][0-9]{0,4}$') -or ([int]$Port -gt 65535))) { Die 'TERMLYX_PORT must be a number from 1 to 65535' }
$lines = @()
if ($env:TERMLYX_IMAGE) { $lines += "TERMLYX_IMAGE=$($env:TERMLYX_IMAGE)" }
if ($Port) { $lines += "TERMLYX_PORT=$Port" }
if ($lines) { Set-Content -Path $Vars -Value $lines -Encoding ascii -ErrorAction Stop }
else { Remove-Item $Vars -ErrorAction SilentlyContinue }
if (-not $env:TERMLYX_IMAGE) {
    docker compose pull
    if ($LASTEXITCODE -ne 0) { Die 'docker compose pull failed' }
}
docker compose up -d
if ($LASTEXITCODE -ne 0) { Die 'docker compose up failed' }

Write-Host 'Waiting for the server to start (up to 2 minutes)...'
$status = ''
for ($i = 0; $i -lt 60; $i++) {
    $status = (docker inspect -f '{{.State.Health.Status}}' termlyx 2>$null)
    if ($status -eq 'healthy') { break }
    Start-Sleep -Seconds 2
}
if ($status -ne 'healthy') { docker logs --tail 40 termlyx; Die 'the server did not become healthy in 2 minutes (log above)' }

Write-Host ''
# Status 3 means the server already has an account: an update, not a failure.
# Start-Process keeps stdout (the banner) on the console and stderr in a file, without
# 5.1 turning stderr lines into error records.
$errFile = [System.IO.Path]::GetTempFileName()
try {
    $p = Start-Process -FilePath docker -ArgumentList 'exec termlyx termlyx-server setup-code' -NoNewWindow -Wait -PassThru -RedirectStandardError $errFile -ErrorAction Stop
    $code = $p.ExitCode
    $err = "$(Get-Content -Raw $errFile -ErrorAction SilentlyContinue)".Trim()
} finally { Remove-Item $errFile -ErrorAction SilentlyContinue }
if ($code -eq 3) { Write-Host 'Server updated.' }
elseif ($code -ne 0) { Die "could not read the setup code (exit status $code): $err" }

Write-Host ''
Write-Host 'Reach it from the Termlyx app at one of:'
Get-NetIPAddress -AddressFamily IPv4 |
    Where-Object { $_.IPAddress -notlike '127.*' -and $_.IPAddress -notlike '169.254.*' -and $_.InterfaceAlias -notlike 'vEthernet*' } |
    ForEach-Object { Write-Host "  https://$($_.IPAddress):$(if ($Port) { $Port } else { 8443 })" }
} finally { Pop-Location }
} catch { Write-Host "error: $($_.Exception.Message)" -ForegroundColor Red }
