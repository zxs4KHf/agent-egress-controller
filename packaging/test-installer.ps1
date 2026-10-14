$ErrorActionPreference = "Stop"
$compiler = Get-Command makensis.exe -ErrorAction SilentlyContinue
if (-not $compiler) { throw "makensis.exe is required for the installer smoke test" }
$temp = Join-Path ([IO.Path]::GetTempPath()) ("aec-installer-test-" + [guid]::NewGuid().ToString('N'))
$payload = Join-Path $temp 'package'
$output = Join-Path $temp 'AgentEgressController-test-setup.exe'
New-Item -ItemType Directory -Path $payload | Out-Null
try {
    foreach ($file in @('aec-tray.exe', 'aec-dev.exe', 'aec-relay.exe', 'aec-agent.exe', 'aec-control.exe', 'README.md', 'LICENSE', 'aec-update.ps1', 'portable.flag')) {
        [IO.File]::WriteAllText((Join-Path $payload $file), 'installer smoke fixture')
    }
    $script = Join-Path $PSScriptRoot 'installer.nsi'
    & $compiler.Source "/DPackageDir=$payload" '/DProductVersion=0.5.0' "/DInstallerOut=$output" $script
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath $output -PathType Leaf)) {
        throw "NSIS failed to compile the installer (exit $LASTEXITCODE)"
    }
    if ((Get-Item -LiteralPath $output).Length -lt 10000) { throw 'compiled installer is unexpectedly small' }
    Write-Output 'Installer smoke test passed: NSIS produced a Windows installer executable.'
} finally {
    Remove-Item -LiteralPath $temp -Recurse -Force -ErrorAction SilentlyContinue
}
