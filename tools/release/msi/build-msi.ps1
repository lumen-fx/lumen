<#
.SYNOPSIS
    Builds the Lumen MSI from a staged install tree.

.DESCRIPTION
    Wraps `wix build` over lumen.wxs, which sits beside this script. The stage
    directory is the exact tree the installer ships:

        <StageDir>\bin\lumenc.exe
        <StageDir>\bin\lumen.dll
        <StageDir>\bin\lumen-launcher.exe
        <StageDir>\bin\std-<hash>.dll
        <StageDir>\share\lumen\lumen.receipt

    Both .github/workflows/release.yml and .github/workflows/msi-smoke.yml
    call this script, so the package a pull request smoke-tests is built the
    same way as the one a tag publishes.

    Requires the WiX command line, which CI installs with
    `dotnet tool install --global wix --version 6.0.1`.

.PARAMETER Version
    The product version, x.y.z. Windows Installer caps the fields at 255, 255
    and 65535, so this script rejects anything larger before wix does.

.PARAMETER StageDir
    The directory holding the tree above. Relative paths are resolved against
    the current directory; wix is handed an absolute path, because it resolves
    File/@Source against its own working directory, not against the .wxs.

.PARAMETER OutFile
    Where to write the .msi.

.EXAMPLE
    tools\release\msi\build-msi.ps1 -Version 0.1.0 -StageDir msi-stage -OutFile lumen-windows-x86_64.msi
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string] $Version,
    [Parameter(Mandatory = $true)][string] $StageDir,
    [Parameter(Mandatory = $true)][string] $OutFile
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# --- validate the version -----------------------------------------------------

if ($Version -notmatch '^\d+\.\d+\.\d+$') {
    throw "build-msi.ps1: -Version must be x.y.z, got '$Version'."
}
$fields = $Version.Split('.')
if ([int]$fields[0] -gt 255 -or [int]$fields[1] -gt 255 -or [int]$fields[2] -gt 65535) {
    throw "build-msi.ps1: '$Version' is out of range for an MSI product version (major and minor at most 255, patch at most 65535)."
}

# --- validate the stage -------------------------------------------------------

if (-not (Test-Path -LiteralPath $StageDir -PathType Container)) {
    throw "build-msi.ps1: -StageDir '$StageDir' is not a directory."
}
$stage = (Resolve-Path -LiteralPath $StageDir).ProviderPath

foreach ($relative in 'bin\lumenc.exe', 'bin\lumen.dll', 'bin\lumen-launcher.exe', 'share\lumen\lumen.receipt') {
    $payload = Join-Path $stage $relative
    if (-not (Test-Path -LiteralPath $payload -PathType Leaf)) {
        throw "build-msi.ps1: the stage is missing $relative (looked for $payload)."
    }
}

# The shared Rust standard library lumen.dll was linked against. Its name
# carries the identity of the compiler that produced it, so it is discovered
# here and handed to wix rather than written into the .wxs, where it would go
# stale the next time the toolchain moves.
$sharedStd = Get-ChildItem -Path (Join-Path $stage 'bin\std-*.dll') | Select-Object -First 1
if (-not $sharedStd) {
    throw "build-msi.ps1: the stage has no bin\std-*.dll. lumen.dll cannot load without it."
}

# --- resolve the output -------------------------------------------------------

$out = [System.IO.Path]::GetFullPath([System.IO.Path]::Combine((Get-Location).ProviderPath, $OutFile))
$outDir = Split-Path -Parent $out
if (-not (Test-Path -LiteralPath $outDir -PathType Container)) {
    New-Item -ItemType Directory -Force -Path $outDir | Out-Null
}

$wxs = Join-Path $PSScriptRoot 'lumen.wxs'
if (-not (Test-Path -LiteralPath $wxs -PathType Leaf)) {
    throw "build-msi.ps1: cannot find $wxs."
}

# --- build --------------------------------------------------------------------

# Full MSI validation, with nothing suppressed. If a change to lumen.wxs ever
# trips an internal consistency evaluator, add that one check as a -sice:<ID>
# flag with a line saying why it does not apply. Never pass -sval, which turns
# validation off wholesale and lets a real packaging error through.

Write-Host "building $out (version $Version) from $stage"

& wix build `
    -arch x64 `
    -d "Version=$Version" `
    -d "StageDir=$stage" `
    -d "SharedStd=$($sharedStd.Name)" `
    -o $out `
    $wxs

if ($LASTEXITCODE -ne 0) {
    throw "build-msi.ps1: wix build failed with exit code $LASTEXITCODE."
}
if (-not (Test-Path -LiteralPath $out -PathType Leaf)) {
    throw "build-msi.ps1: wix build reported success but $out does not exist."
}

Write-Host "wrote $out"
