# Creative Console Daemon - Install Scheduled Task
# Registers a Task Scheduler task that runs restart.ps1 hidden at user logon.
# Usage: .\install-task.ps1 [-TaskName <name>] [-Config <path>]

param(
    [string]$TaskName = "CreativeConsoleDaemon",
    [string]$Config = (Join-Path $PSScriptRoot "config.toml")
)

$ErrorActionPreference = "Stop"

$restartScript = Join-Path $PSScriptRoot "restart.ps1"
if (-not (Test-Path $restartScript)) {
    Write-Error "restart.ps1 not found at $restartScript"
    exit 1
}

if (-not (Test-Path $Config)) {
    Write-Warning "Config not found at $Config (task will still install, but daemon will fail until config exists)."
}

$argument = "-WindowStyle Hidden -ExecutionPolicy Bypass -File `"$restartScript`" -Config `"$Config`""

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
