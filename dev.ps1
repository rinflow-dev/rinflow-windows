# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

<#
.SYNOPSIS
Builds, runs and stops Rinflow for Windows while developing.

.DESCRIPTION
Run it directly (.\dev.ps1 run). A Makefile can call it too.

  build  Builds only (build.ps1). A running agent or settings window keeps
         running: files it holds open are moved aside, not overwritten.
  run    Stops every running copy, then builds, then starts the agent in
         this console, where its log appears, and opens the settings
         window. Ctrl+C quits the agent. Stopping comes first so a running
         window cannot hold files the build must replace, and an old copy
         never survives a failed build: after one, nothing is running.
  stop   Quits every running settings window and agent: politely first,
         as the tray menu does, then by force.

Keep this file ASCII-only: Windows PowerShell 5.1 reads scripts without a
BOM in the system code page.

.EXAMPLE
.\dev.ps1 run -Configuration Release
#>
[CmdletBinding()]
param(
    [Parameter(Position = 0)]
    [ValidateSet('build', 'run', 'stop')]
    [string]$Action = 'build',

    [ValidateSet('Debug', 'Release')]
    [string]$Configuration = 'Debug',

    [ValidateSet('x64', 'ARM64')]
    [string]$Platform = 'x64'
)

$ErrorActionPreference = 'Stop'

# A 32-bit make (such as the winget one) starts the 32-bit PowerShell, which
# sees a WOW64 view of the system. Continue in the native one.
if ($env:PROCESSOR_ARCHITEW6432 -and -not [Environment]::Is64BitProcess) {
    $native = Join-Path $env:windir 'Sysnative\WindowsPowerShell\v1.0\powershell.exe'
    & $native -NoProfile -ExecutionPolicy Bypass -File $PSCommandPath $Action `
        -Configuration $Configuration -Platform $Platform
    exit $LASTEXITCODE
}

$output = Join-Path $PSScriptRoot "app\$Platform\$Configuration\Rinflow"

Add-Type -Namespace RinflowDev -Name Win32 -MemberDefinition @'
[DllImport("user32.dll", CharSet = CharSet.Unicode)]
public static extern IntPtr FindWindowW(string className, string windowName);
[DllImport("user32.dll")]
public static extern bool PostMessageW(IntPtr window, uint message, IntPtr wParam, IntPtr lParam);
[DllImport("kernel32.dll", CharSet = CharSet.Unicode)]
public static extern bool WaitNamedPipeW(string name, uint timeout);
'@

# Waits for the processes to exit, then kills the ones still running.
function Wait-Exit($Processes, [int]$Seconds) {
    foreach ($process in $Processes) {
        if (-not $process.WaitForExit($Seconds * 1000)) {
            Write-Host "Killing $($process.ProcessName) ($($process.Id))"
            Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
            $process.WaitForExit(5000) | Out-Null
        }
    }
}

function Stop-Rinflow {
    # The settings window first: it would start a new agent otherwise.
    $windows = @(Get-Process -Name 'Rinflow' -ErrorAction SilentlyContinue)
    foreach ($window in $windows) {
        Write-Host "Closing the settings window ($($window.Id))"
        $window.CloseMainWindow() | Out-Null
    }
    Wait-Exit $windows 3

    # One agent runs per session. Its hidden tray window takes WM_CLOSE like
    # the tray menu's Quit: the icon goes away and peers see a clean
    # disconnect, which killing it would not give.
    $agents = @(Get-Process -Name 'rinflow-agent' -ErrorAction SilentlyContinue)
    if ($agents.Count -gt 0) {
        Write-Host "Quitting rinflow-agent ($(@($agents.Id) -join ', '))"
        # [NullString]: PowerShell would turn $null into "", a window title.
        $tray = [RinflowDev.Win32]::FindWindowW('RinflowAgentTray', [NullString]::Value)
        if ($tray -ne [IntPtr]::Zero) {
            [RinflowDev.Win32]::PostMessageW($tray, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null
        }
    }
    Wait-Exit $agents 5

    # Files a build moved aside while these were running are free now.
    if (Test-Path $output) {
        Get-ChildItem $output -Filter '*.old' -File | Remove-Item -ErrorAction SilentlyContinue
    }
}

function Invoke-Build {
    & (Join-Path $PSScriptRoot 'build.ps1') -Configuration $Configuration -Platform $Platform
    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }
}

function Start-Rinflow {
    $agent = Start-Process -FilePath (Join-Path $output 'rinflow-agent.exe') `
        -WorkingDirectory $output -NoNewWindow -PassThru

    # Open the settings window once the agent's pipe exists.
    $pipe = '\\.\pipe\Rinflow.Agent.' + [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    $deadline = (Get-Date).AddSeconds(10)
    while (-not $agent.HasExited -and (Get-Date) -lt $deadline -and
        -not [RinflowDev.Win32]::WaitNamedPipeW($pipe, 1)) {
        Start-Sleep -Milliseconds 100
    }
    if ($agent.HasExited) {
        Write-Host 'rinflow-agent exited during start-up'
        exit 1
    }
    # The window starts an agent of its own when it loses this one. This
    # agent belongs to the terminal, so point that override at nothing.
    $env:RINFLOW_AGENT_PATH = 'started-by-make-run'
    Start-Process -FilePath (Join-Path $output 'Rinflow.exe') -WorkingDirectory $output
    Remove-Item Env:RINFLOW_AGENT_PATH

    Write-Host 'rinflow-agent is running; its log follows. Ctrl+C quits it.'
    $agent.WaitForExit()
}

switch ($Action) {
    'build' { Invoke-Build }
    'stop' { Stop-Rinflow }
    'run' {
        Stop-Rinflow
        Invoke-Build
        Start-Rinflow
    }
}
