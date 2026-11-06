# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

<#
.SYNOPSIS
Downloads and starts the latest Rinflow setup program for this PC.

.DESCRIPTION
Finds out whether Windows runs on an Intel/AMD (x64) or an ARM64 processor,
downloads the matching setup program from the latest GitHub release, checks
its SHA-256 against the value GitHub publishes for it, and starts it. The
setup program asks for administrator approval itself.

The file is marked as downloaded from the internet, as a browser would: the
setup program is not code-signed yet, so SmartScreen warns before it runs.

Keep this file ASCII-only: Windows PowerShell 5.1 reads scripts without a
BOM in the system code page.

.EXAMPLE
irm https://raw.githubusercontent.com/rinflow-dev/rinflow-windows/main/scripts/install.ps1 | iex
#>

function Get-RinflowArchitecture {
    # The processor Windows runs on, not the one this PowerShell runs as: an
    # x64 PowerShell on an ARM64 PC reports AMD64 in its environment.
    try {
        Add-Type -Namespace RinflowSetup -Name Native -MemberDefinition @'
[DllImport("kernel32.dll")]
public static extern bool IsWow64Process2(IntPtr process, out ushort processMachine, out ushort nativeMachine);
'@ -ErrorAction Stop
        $process = [uint16]0
        $native = [uint16]0
        $handle = [System.Diagnostics.Process]::GetCurrentProcess().Handle
        if ([RinflowSetup.Native]::IsWow64Process2($handle, [ref]$process, [ref]$native)) {
            switch ($native) {
                0xAA64 { return 'arm64' }
                0x8664 { return 'x64' }
                default { throw "Rinflow needs a 64-bit Intel, AMD or ARM processor (machine type 0x$($native.ToString('X')))." }
            }
        }
    } catch [System.EntryPointNotFoundException] {
        # Before Windows 10 version 1709.
    }
    $name = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
    switch ($name) {
        'ARM64' { return 'arm64' }
        'AMD64' { return 'x64' }
        default { throw "Rinflow needs a 64-bit Intel, AMD or ARM processor, not $name." }
    }
}

function Install-Rinflow {
    $ErrorActionPreference = 'Stop'
    # Windows PowerShell 5.1 does not use TLS 1.2 by default, which GitHub needs.
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
    # The progress bar makes Invoke-WebRequest much slower in Windows PowerShell.
    $ProgressPreference = 'SilentlyContinue'

    $architecture = Get-RinflowArchitecture
    Write-Host "This PC: $architecture"

    $release = Invoke-RestMethod -Uri 'https://api.github.com/repos/rinflow-dev/rinflow-windows/releases/latest' `
        -Headers @{ Accept = 'application/vnd.github+json'; 'User-Agent' = 'rinflow-install' }
    $asset = $release.assets | Where-Object { $_.name -match "^Rinflow-Setup-.+-$architecture\.exe$" } | Select-Object -First 1
    if (-not $asset) { throw "Release $($release.tag_name) has no setup program for $architecture." }

    $folder = Join-Path ([IO.Path]::GetTempPath()) ('rinflow-' + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $folder | Out-Null
    try {
        $file = Join-Path $folder $asset.name
        Write-Host "Downloading $($asset.name) ($([math]::Round($asset.size / 1MB, 1)) MB)"
        Invoke-WebRequest -Uri $asset.browser_download_url -OutFile $file -UseBasicParsing

        $expected = if ($asset.digest) { $asset.digest -replace '^sha256:', '' } else { $null }
        if ($expected) {
            $actual = (Get-FileHash -Algorithm SHA256 -Path $file).Hash
            if ($actual -ne $expected) { throw "The download does not match its SHA-256 ($actual, expected $expected)." }
            Write-Host 'SHA-256 matches.'
        } else {
            Write-Warning 'GitHub published no SHA-256 for this file, so it was not checked.'
        }

        # As a browser would: the setup program is unsigned, so Windows warns.
        Set-Content -Path $file -Stream Zone.Identifier -Value "[ZoneTransfer]`r`nZoneId=3" -ErrorAction SilentlyContinue

        Write-Host 'Starting the setup program...'
        Start-Process -FilePath $file -Wait
    } finally {
        Remove-Item -LiteralPath $folder -Recurse -Force -ErrorAction SilentlyContinue
    }
}

Install-Rinflow
