# Creative Console Daemon - Uninstall Scheduled Task
# Usage: .\uninstall-task.ps1 [-TaskName <name>]

param(
    [string]$TaskName = "CreativeConsoleDaemon"
)

$ErrorActionPreference = "Stop"

if (-not (Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue)) {
    Write-Host "Task '$TaskName' not registered. Nothing to do."
    exit 0
}

Stop-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false
Write-Host "Task '$TaskName' removed."
