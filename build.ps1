<#
.SYNOPSIS
Builds LinkCursor for Windows: the WinUI settings app and the Rust agent.

.DESCRIPTION
Works from any PowerShell window. Loads the Visual Studio developer
environment, switches the console to UTF-8 so MSBuild, MSVC and Cargo output
is decoded the same way whatever the system locale, and runs MSBuild on
app\LinkCursor.vcxproj. Keep this file ASCII-only: Windows PowerShell 5.1 reads
scripts without a BOM in the system code page.

.EXAMPLE
.\build.cmd
.\build.cmd -Configuration Release
.\build.cmd -Language en-US
#>
[CmdletBinding()]
param(
    [ValidateSet('Debug', 'Release')]
    [string]$Configuration = 'Debug',

    [ValidateSet('x64', 'ARM64')]
    [string]$Platform = 'x64',

    # Language of MSBuild and compiler messages, such as zh-CN or en-US.
    # MSBuild includes most languages. The C++ compiler only speaks the
    # Visual Studio language packs that are installed (see README.md).
    [string]$Language = 'zh-CN',

    [switch]$SkipAgent
)

$ErrorActionPreference = 'Stop'

# The system code page is not necessarily UTF-8 (it is 932 with a Japanese
# system locale, for example). Make this console and every tool started from
# it read and write UTF-8 so nothing is decoded with the wrong code page.
$utf8 = [System.Text.UTF8Encoding]::new($false)
[Console]::OutputEncoding = $utf8
[Console]::InputEncoding = $utf8
$OutputEncoding = $utf8

# MSBuild and NuGet read DOTNET_CLI_UI_LANGUAGE; cl.exe and link.exe read VSLANG.
$env:DOTNET_CLI_UI_LANGUAGE = $Language
$env:VSLANG = [System.Globalization.CultureInfo]::GetCultureInfo($Language).LCID

$installer = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer'
$vswhere = Join-Path $installer 'vswhere.exe'
if (-not (Test-Path $vswhere)) {
    throw 'Visual Studio Installer was not found. See README.md for the prerequisites.'
}
$vsPath = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if (-not $vsPath) {
    throw 'No Visual Studio or Build Tools installation with the C++ tools was found. See README.md.'
}

# VsDevCmd.bat runs vswhere.exe from the installer folder by name. With
# NoDefaultCurrentDirectoryInExePath set, cmd does not look in the current
# folder, so put the installer on PATH instead.
$env:Path = "$installer;$env:Path"

$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
if ((Test-Path $cargoBin) -and -not ($env:Path -split ';' -contains $cargoBin)) {
    $env:Path = "$cargoBin;$env:Path"
}

$hostArch = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'arm64' } else { 'amd64' }
$targetArch = if ($Platform -eq 'ARM64') { 'arm64' } else { 'amd64' }
Import-Module (Join-Path $vsPath 'Common7\Tools\Microsoft.VisualStudio.DevShell.dll')
Enter-VsDevShell -VsInstallPath $vsPath -SkipAutomaticLocation `
    -DevCmdArguments "-arch=$targetArch -host_arch=$hostArch -no_logo" | Out-Null

$arguments = @(
    (Join-Path $PSScriptRoot 'app\LinkCursor.vcxproj'),
    '-restore',
    "-p:Configuration=$Configuration",
    "-p:Platform=$Platform",
    '-nologo',
    '-verbosity:minimal'
)
if ($SkipAgent) {
    $arguments += '-p:SkipAgentBuild=true'
}

& msbuild @arguments
exit $LASTEXITCODE
