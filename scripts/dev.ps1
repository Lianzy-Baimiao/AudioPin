param([ValidateSet('check','test','build','run','clippy','fmt')][string]$Action = 'build', [switch]$Release, [string[]]$ExtraArgs)
$ErrorActionPreference = 'Stop'
Set-Location (Split-Path $PSScriptRoot -Parent)
# Use MSVC; honor an existing Developer PowerShell on other machines.
# This host has an older Visual Studio installed on E: whose SDK discovery is stale.
$vs = 'E:\VS\VC\Tools\MSVC\14.28.29333'
$sdk = 'E:\Windows Kits\10'
if (Test-Path "$vs\bin\Hostx64\x64\cl.exe") {
    $env:PATH = "$vs\bin\Hostx64\x64;$sdk\bin\10.0.22000.0\x64;$env:PATH"
    $env:LIB = "$vs\lib\x64;$sdk\Lib\10.0.22000.0\um\x64;$sdk\Lib\10.0.22000.0\ucrt\x64"
    $env:INCLUDE = "$vs\include;$sdk\Include\10.0.22000.0\ucrt;$sdk\Include\10.0.22000.0\shared;$sdk\Include\10.0.22000.0\um"
}
$env:CARGO_TARGET_DIR = Join-Path $env:USERPROFILE '.cargo-target\audiopin-msvc'
$cargoArgs = @('+stable-x86_64-pc-windows-msvc', $Action)
if ($Release -and $Action -in @('build','run','test')) { $cargoArgs += '--release' }
if ($ExtraArgs) { $cargoArgs += $ExtraArgs }
& cargo @cargoArgs
exit $LASTEXITCODE
