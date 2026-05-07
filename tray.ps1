# Creative Console Daemon - System Tray Wrapper
# Runs the daemon as a child process and shows a tray icon with controls.
# Restarts on exit code 2 (device disconnect). Replaces restart.ps1 when
# you want a tray icon. Use restart.ps1 for headless supervision.
#
# Usage: .\tray.ps1 [-Config <path>]

param(
    [string]$Config = (Join-Path $PSScriptRoot "config.toml")
)

Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing

$ErrorActionPreference = "Stop"

$exe = Join-Path $PSScriptRoot "target\release\creative-console-daemon.exe"
if (-not (Test-Path $exe)) {
    $exe = Join-Path $PSScriptRoot "target\debug\creative-console-daemon.exe"
}
if (-not (Test-Path $exe)) {
    [System.Windows.Forms.MessageBox]::Show("Daemon binary not found. Run 'cargo build --release' first.", "Creative Console", "OK", "Error") | Out-Null
    exit 1
}

$logOut = Join-Path $PSScriptRoot "daemon.log"
$logErr = Join-Path $PSScriptRoot "daemon.err.log"
$script:proc = $null
$script:userQuit = $false

function Start-Daemon {
    if ($script:proc -and -not $script:proc.HasExited) { return }
    $script:proc = Start-Process -FilePath $exe `
        -ArgumentList "--config", "`"$Config`"" `
        -WindowStyle Hidden `
        -PassThru `
        -RedirectStandardOutput $logOut `
        -RedirectStandardError $logErr
    $tray.Text = "Creative Console (running, PID $($script:proc.Id))"
    $tray.Icon = [System.Drawing.SystemIcons]::Application
}

function Stop-Daemon {
    if ($script:proc -and -not $script:proc.HasExited) {
        try { Stop-Process -Id $script:proc.Id -Force -ErrorAction Stop } catch { }
    }
    $tray.Text = "Creative Console (stopped)"
    $tray.Icon = [System.Drawing.SystemIcons]::Information
}

$tray = New-Object System.Windows.Forms.NotifyIcon
$tray.Icon = [System.Drawing.SystemIcons]::Application
$tray.Visible = $true
$tray.Text = "Creative Console"

$menu = New-Object System.Windows.Forms.ContextMenuStrip

$miStatus = $menu.Items.Add("Status: starting...")
$miStatus.Enabled = $false

$menu.Items.Add("-") | Out-Null

$miRestart = $menu.Items.Add("Restart daemon")
$miRestart.Add_Click({
    Stop-Daemon
    Start-Sleep -Milliseconds 500
    Start-Daemon
})

$miOpenLog = $menu.Items.Add("Open log")
$miOpenLog.Add_Click({
    if (Test-Path $logOut) { Start-Process notepad.exe $logOut } else {
        [System.Windows.Forms.MessageBox]::Show("No log yet at $logOut", "Creative Console") | Out-Null
    }
})

$miEditConfig = $menu.Items.Add("Edit config")
$miEditConfig.Add_Click({ Start-Process notepad.exe $Config })

$miOpenFolder = $menu.Items.Add("Open folder")
$miOpenFolder.Add_Click({ Start-Process explorer.exe $PSScriptRoot })

$menu.Items.Add("-") | Out-Null

$miQuit = $menu.Items.Add("Quit")
$miQuit.Add_Click({
    $script:userQuit = $true
    Stop-Daemon
    $tray.Visible = $false
    [System.Windows.Forms.Application]::Exit()
})

$tray.ContextMenuStrip = $menu
$tray.Add_MouseDoubleClick({ $miRestart.PerformClick() })

Start-Daemon

# Watchdog: restart on exit code 2 (device disconnect)
$timer = New-Object System.Windows.Forms.Timer
$timer.Interval = 1000
$timer.Add_Tick({
    if ($script:userQuit) { return }
    if ($script:proc -and $script:proc.HasExited) {
        $code = $script:proc.ExitCode
        if ($code -eq 2) {
            $miStatus.Text = "Status: device disconnected, restarting..."
            $tray.Text = "Creative Console (restarting)"
            Start-Sleep -Milliseconds 1500
            Start-Daemon
        } else {
            $miStatus.Text = "Status: stopped (exit $code)"
            $tray.Text = "Creative Console (stopped, exit $code)"
        }
    } elseif ($script:proc) {
        $miStatus.Text = "Status: running (PID $($script:proc.Id))"
    }
})
$timer.Start()

# Clean shutdown on console close / logoff
$null = Register-EngineEvent PowerShell.Exiting -Action {
    if ($script:proc -and -not $script:proc.HasExited) {
        Stop-Process -Id $script:proc.Id -Force -ErrorAction SilentlyContinue
    }
    $tray.Visible = $false
}

[System.Windows.Forms.Application]::Run()
