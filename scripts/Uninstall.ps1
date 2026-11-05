$ErrorActionPreference = 'Stop'
$expected = [IO.Path]::GetFullPath((Join-Path $env:LOCALAPPDATA 'Programs\AudioPin'))
$actual = [IO.Path]::GetFullPath($PSScriptRoot)
if ($actual.TrimEnd('\') -ne $expected.TrimEnd('\')) { throw 'Run Uninstall.ps1 from the installed AudioPin directory.' }
$exe = Join-Path $actual 'AudioPin.exe'
Get-Process audiopin -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $exe } | ForEach-Object { Stop-Process -Id $_.Id; $_.WaitForExit() }
$run = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$value = (Get-ItemProperty -LiteralPath $run -Name AudioPin -ErrorAction SilentlyContinue).AudioPin
if ($value -like ('"' + $exe + '"*')) { Remove-ItemProperty -LiteralPath $run -Name AudioPin }
$shortcut = Join-Path ([Environment]::GetFolderPath('Programs')) 'AudioPin.lnk'
if (Test-Path -LiteralPath $shortcut) { Remove-Item -LiteralPath $shortcut }
# Delete only known application files, never the data/config directory.
foreach ($name in @('AudioPin.exe','README.md','LICENSE','config.example.toml','Uninstall.ps1')) {
    $file = Join-Path $actual $name
    if (Test-Path -LiteralPath $file) { Remove-Item -LiteralPath $file }
}
Write-Output 'AudioPin removed. Settings and logs were retained in %LOCALAPPDATA%\AudioPin.'
