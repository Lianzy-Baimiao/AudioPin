param([string]$Exe = (Join-Path $env:LOCALAPPDATA 'Programs\AudioPin\AudioPin.exe'))
$ErrorActionPreference = 'Stop'
# Explicit fault injection: briefly interrupts AMD microphone processing.
# Run with AudioPin enabled and when a short audio interruption is acceptable.
$amdExe = Join-Path $env:SystemRoot 'System32\AMD\ANR\AMDNoiseSuppression.exe'
$expectedHash = '4ecec1b76d9ef18c7f2a26da823e71124db627e3acd3c178c89b80b3aeac20ee'
if ((Get-FileHash -LiteralPath $amdExe -Algorithm SHA256).Hash.ToLowerInvariant() -ne $expectedHash) {
    throw 'Unverified AMD backend. No fault injected.'
}
function Read-Diagnostic {
    $child = Start-Process -FilePath $Exe -ArgumentList '--diagnose' -WindowStyle Hidden -PassThru
    if (-not $child.WaitForExit(15000)) {
        $child.Kill()
        throw 'AudioPin diagnostics timed out'
    }
    if ($child.ExitCode -ne 0) { throw 'AudioPin diagnostics failed' }
    Get-Content -Raw -LiteralPath (Join-Path $env:LOCALAPPDATA 'AudioPin\diagnostics.json') | ConvertFrom-Json
}
$initial = Read-Diagnostic
if ($initial.guard_enabled -ne $true -or $initial.amd_enabled -ne $true -or $initial.amd_capture_active -ne $true) {
    throw 'Expected AudioPin 0.1.2+, enabled guard and active AMD capture. No fault injected.'
}
$backend = @(Get-Process AMDNoiseSuppression | Where-Object { $_.Path -eq $amdExe })
if ($backend.Count -ne 1) { throw 'Expected one installed AMD backend. No fault injected.' }
$clock = [Diagnostics.Stopwatch]::StartNew()
try {
    Set-ItemProperty -LiteralPath 'HKCU:\Software\AMD\CN\Audio' -Name ANRToggleState -Value 'false'
    $backend | ForEach-Object { Stop-Process -Id $_.Id; $_.WaitForExit() }
    $recovered = $false
    while ($clock.Elapsed.TotalSeconds -lt 30) {
        Start-Sleep -Seconds 1
        $report = Read-Diagnostic
        if ($report.amd_runtime_enabled -eq 'true' -and $report.amd_capture_active -eq $true) {
            $recovered = $true
            break
        }
    }
    if (-not $recovered) { throw 'AMD restart did not restore live capture within 30 seconds' }
    'PASS: global-off/backend-exit recovered in {0:N2}s, PID {1}' -f $clock.Elapsed.TotalSeconds, ($report.amd_process_ids -join ',')
} finally {
    Set-ItemProperty -LiteralPath 'HKCU:\Software\AMD\CN\Audio' -Name ANRToggleState -Value 'true'
    if (-not (Get-Process AMDNoiseSuppression -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $amdExe })) {
        Start-Process -FilePath $amdExe -ArgumentList NoQuit -WorkingDirectory (Split-Path $amdExe) -WindowStyle Hidden
    }
}
