# Creative Console Daemon - Supervisor Script
# Restarts the daemon on exit code 2 (device disconnect).
# Usage: .\restart.ps1

$exe = Join-Path $PSScriptRoot "target\release\creative-console-daemon.exe"
if (-not (Test-Path $exe)) {
    $exe = Join-Path $PSScriptRoot "target\debug\creative-console-daemon.exe"
}
if (-not (Test-Path $exe)) {
    Write-Error "Daemon binary not found. Run 'cargo build --release' first."
    exit 1
}

while ($true) {
    Write-Host "[supervisor] Starting daemon..."
    & $exe --config (Join-Path $PSScriptRoot "config.toml")
    $code = $LASTEXITCODE

    if ($code -ne 2) {
        Write-Host "[supervisor] Daemon exited with code $code. Stopping."
        break
    }

    Write-Host "[supervisor] Device disconnected (exit code 2). Waiting 2 seconds before restart..."
    Start-Sleep -Seconds 2
}
