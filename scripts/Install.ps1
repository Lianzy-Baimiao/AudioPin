param([switch]$NoAutostart, [switch]$NoLaunch)
$ErrorActionPreference = 'Stop'
$source = Join-Path $PSScriptRoot 'AudioPin.exe'
if (-not (Test-Path -LiteralPath $source)) { throw 'Run this script from the extracted release package.' }
$installDir = Join-Path $env:LOCALAPPDATA 'Programs\AudioPin'
$destination = Join-Path $installDir 'AudioPin.exe'
New-Item -ItemType Directory -Force -Path $installDir | Out-Null
Get-Process audiopin -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $destination } | ForEach-Object { Stop-Process -Id $_.Id; $_.WaitForExit() }
foreach ($file in @('AudioPin.exe','README.md','LICENSE','config.example.toml','Uninstall.ps1')) {
    $item = Join-Path $PSScriptRoot $file
    if (Test-Path -LiteralPath $item) { Copy-Item -LiteralPath $item -Destination (Join-Path $installDir $file) -Force }
}
$shell = New-Object -ComObject WScript.Shell
$shortcutPath = Join-Path ([Environment]::GetFolderPath('Programs')) 'AudioPin.lnk'
$shortcut = $shell.CreateShortcut($shortcutPath)
$shortcut.TargetPath = $destination
$shortcut.WorkingDirectory = $installDir
$shortcut.Description = 'AudioPin - Windows audio guardian'
$shortcut.Save()
if (-not $NoAutostart) {
    New-ItemProperty -Path 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name AudioPin -Value ('"' + $destination + '" --background') -PropertyType String -Force | Out-Null
}
if (-not $NoLaunch) { Start-Process -FilePath $destination -WindowStyle Hidden }
Write-Output "Installed: $destination"
Write-Output 'Settings and logs: %LOCALAPPDATA%\AudioPin'
