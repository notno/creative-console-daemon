# Creative Console Daemon - Install Scheduled Task
# Registers a Task Scheduler task that runs the supervisor hidden at user logon.
# Default supervisor is tray.ps1 (system tray icon + watchdog).
# Pass -Headless to use restart.ps1 instead (no tray, console-style).
#
# Usage: .\install-task.ps1 [-TaskName <name>] [-Config <path>] [-Headless]

param(
    [string]$TaskName = "CreativeConsoleDaemon",
    [string]$Config = (Join-Path $PSScriptRoot "config.toml"),
    [switch]$Headless
)

$ErrorActionPreference = "Stop"

$scriptName = if ($Headless) { "restart.ps1" } else { "tray.ps1" }
$supervisorScript = Join-Path $PSScriptRoot $scriptName
if (-not (Test-Path $supervisorScript)) {
    Write-Error "$scriptName not found at $supervisorScript"
    exit 1
}

if (-not (Test-Path $Config)) {
    Write-Warning "Config not found at $Config (task will still install, but daemon will fail until config exists)."
}

$argument = "-WindowStyle Hidden -ExecutionPolicy Bypass -File `"$supervisorScript`" -Config `"$Config`""

$action = New-ScheduledTaskAction `
    -Execute "powershell.exe" `
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
