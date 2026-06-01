# Creative Console - Install Scheduled Task
# Registers a Task Scheduler task that starts the app at user logon.
# Default is editor.exe: a windowless tray supervisor (auto-restart watchdog)
# with the button/page editor reachable from the tray menu ("Open editor").
#
# Pass one or more configs — one daemon runs per config, so the MX Creative and
# the Stream Deck can both run at once. Devices that aren't plugged in just
# leave their daemon stopped.
#
# Pass -Headless to use restart.ps1 instead (no tray/editor, console supervisor;
# single config only — uses the first one given).
#
# Usage: .\install-task.ps1 [-TaskName <name>] [-Configs <path>[,<path>...]] [-Headless]

param(
    [string]$TaskName = "CreativeConsoleDaemon",
    [string[]]$Configs = @(
        (Join-Path $PSScriptRoot "config.ctrl-win.toml"),
        (Join-Path $PSScriptRoot "config.streamdeck.toml")
    ),
    [switch]$Headless
)

$ErrorActionPreference = "Stop"

if ($Headless) {
    $supervisorScript = Join-Path $PSScriptRoot "restart.ps1"
    if (-not (Test-Path $supervisorScript)) {
        Write-Error "restart.ps1 not found at $supervisorScript"
        exit 1
    }
    if ($Configs.Count -gt 1) {
        Write-Warning "Headless mode supervises a single config; using '$($Configs[0])'."
    }
    $execute = "powershell.exe"
    $argument = "-WindowStyle Hidden -ExecutionPolicy Bypass -File `"$supervisorScript`" -Config `"$($Configs[0])`""
} else {
    $exe = Join-Path $PSScriptRoot "target\release\editor.exe"
    if (-not (Test-Path $exe)) { $exe = Join-Path $PSScriptRoot "target\debug\editor.exe" }
    if (-not (Test-Path $exe)) {
        Write-Error "editor.exe not found. Run 'cargo build --release --workspace' first."
        exit 1
    }
    $execute = $exe
    $argument = ($Configs | ForEach-Object { "`"$_`"" }) -join ' '
}

foreach ($c in $Configs) {
    if (-not (Test-Path $c)) {
        Write-Warning "Config not found at $c (task will still install, but that daemon will fail until the config exists)."
    }
}

$action = New-ScheduledTaskAction `
    -Execute $execute `
    -Argument $argument `
    -WorkingDirectory $PSScriptRoot

$trigger = New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME

$settings = New-ScheduledTaskSettingsSet `
    -AllowStartIfOnBatteries `
    -DontStopIfGoingOnBatteries `
    -StartWhenAvailable `
    -RestartCount 3 `
    -RestartInterval (New-TimeSpan -Minutes 1) `
    -ExecutionTimeLimit ([TimeSpan]::Zero) `
    -MultipleInstances IgnoreNew

$principal = New-ScheduledTaskPrincipal `
    -UserId $env:USERNAME `
    -LogonType Interactive `
    -RunLevel Limited

if (Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue) {
    Write-Host "Task '$TaskName' already exists. Updating..."
    Set-ScheduledTask -TaskName $TaskName -Action $action -Trigger $trigger -Settings $settings -Principal $principal | Out-Null
} else {
    Register-ScheduledTask `
        -TaskName $TaskName `
        -Action $action `
        -Trigger $trigger `
        -Settings $settings `
        -Principal $principal `
        -Description "Creative Console daemon - runs MX Creative / Stream Deck XL handler at logon" | Out-Null
    Write-Host "Task '$TaskName' registered."
}

Write-Host ""
Write-Host "Installed. Will run at next logon."
Write-Host "Start now:   Start-ScheduledTask -TaskName $TaskName"
Write-Host "Stop:        Stop-ScheduledTask -TaskName $TaskName"
Write-Host "Status:      Get-ScheduledTask -TaskName $TaskName | Get-ScheduledTaskInfo"
Write-Host "Remove:      .\uninstall-task.ps1"
