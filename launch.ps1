# Creative Console - Launch the tray app
# Starts editor.exe: the windowless tray supervisor + button/page editor.
# Use this to relaunch after you've fully quit the app from the tray menu.
# (The "Start daemons" tray item only restarts the daemons; this restarts the app.)
#
# Pass one or more configs — one daemon runs per config, so both the MX
# Creative and the Stream Deck can run at once. Devices that aren't plugged in
# just leave their daemon stopped.
#
# Usage: .\launch.ps1 [-Configs <path>[,<path>...]]

param(
    [string[]]$Configs = @(
        (Join-Path $PSScriptRoot "config.ctrl-win.toml"),
        (Join-Path $PSScriptRoot "config.streamdeck.toml")
    )
)

$exe = Join-Path $PSScriptRoot "target\release\editor.exe"
if (-not (Test-Path $exe)) { $exe = Join-Path $PSScriptRoot "target\debug\editor.exe" }
if (-not (Test-Path $exe)) {
    Write-Error "editor.exe not found. Run 'cargo build --release --workspace' first."
    exit 1
}

$argList = $Configs | ForEach-Object { "`"$_`"" }
Start-Process -FilePath $exe -ArgumentList $argList -WorkingDirectory $PSScriptRoot
Write-Host "Launched Creative Console (tray icon) for: $($Configs -join ', ')"
Write-Host "Right-click the tray icon for controls."
