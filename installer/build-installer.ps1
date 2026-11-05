# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

<#
.SYNOPSIS
Builds the Rinflow setup program: the Release build, then Inno Setup.

.DESCRIPTION
Runs build.ps1 -Configuration Release -EffectDemo Off for the platform, then
compiles Rinflow.iss with ISCC.exe (Inno Setup 6). The version comes from
agent\Cargo.toml. The result is
installer\output\Rinflow-Setup-<version>-<x64|arm64>.exe.
Keep this file ASCII-only: Windows PowerShell 5.1 reads scripts without a
BOM in the system code page.

.EXAMPLE
.\installer\build-installer.ps1
.\installer\build-installer.ps1 -Platform ARM64
.\installer\build-installer.ps1 -SkipBuild
#>
[CmdletBinding()]
param(
    [ValidateSet('x64', 'ARM64')]
    [string]$Platform = 'x64',

    # Language of build messages, passed to build.ps1.
    [string]$Language = 'zh-CN',

    # Package what is already in app\<platform>\Release\Rinflow.
    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent

$iscc = @(
    "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
    "$env:ProgramFiles\Inno Setup 6\ISCC.exe",
    "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe"
) | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $iscc) {
    throw 'Inno Setup 6 was not found. Install it with: winget install JRSoftware.InnoSetup'
}

if (-not $SkipBuild) {
    # Setup never carries the edge animation's tuning page.
    & (Join-Path $root 'build.ps1') -Configuration Release -Platform $Platform -Language $Language -EffectDemo Off
    if ($LASTEXITCODE -ne 0) {
        throw "The Release build failed ($LASTEXITCODE)."
    }
}

$cargo = Get-Content (Join-Path $root 'agent\Cargo.toml') -Raw
if ($cargo -notmatch '(?m)^version\s*=\s*"([^"]+)"') {
    throw 'No version in agent\Cargo.toml.'
}
$version = $Matches[1]
$arch = $Platform.ToLowerInvariant()
$source = Join-Path $root "app\$Platform\Release\Rinflow"
foreach ($exe in 'Rinflow.exe', 'rinflow-agent.exe', 'rinflow-input.exe', 'rinflow-service.exe') {
    if (-not (Test-Path (Join-Path $source $exe))) {
        throw "$exe is missing from $source."
    }
}

& $iscc "/DAppVersion=$version" "/DArch=$arch" "/DSourceDir=$source" (Join-Path $PSScriptRoot 'Rinflow.iss')
if ($LASTEXITCODE -ne 0) {
    throw "Inno Setup failed ($LASTEXITCODE)."
}
Get-Item (Join-Path $PSScriptRoot "output\Rinflow-Setup-$version-$arch.exe")
