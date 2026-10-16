param([Parameter(Mandatory=$true)][string]$ProbeExe)
$ErrorActionPreference='Stop'
$ProbeExe=(Resolve-Path -LiteralPath $ProbeExe).Path
$sandbox=Join-Path ([IO.Path]::GetTempPath()) ('aec-identity-'+[guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $sandbox | Out-Null
try {
    $report=Join-Path $sandbox 'identity.json'
    # One argument with spaces/Unicode and one ending in backslash exercise the real re-exec argv.
    $unicode=([string][char]0x4E2D)+[char]0x6587+' fixture'
    $child=Start-Process -FilePath $ProbeExe -ArgumentList ('"'+$report+'" "'+$unicode+'" C:\fixture\') -WindowStyle Hidden -PassThru
    if(-not $child.WaitForExit(15000)) {throw 'Identity fixture did not exit normally; not force-killing it'}
    $result=Get-Content -LiteralPath $report -Raw -Encoding UTF8|ConvertFrom-Json
    if($child.ExitCode -ne 0 -or -not $result.independent) {throw 'HOST_IDENTITY_INHERITED: Agent-launched executable retained packaged-host identity'}
    if($result.arguments.Count -ne 2 -or $result.arguments[0] -cne $unicode -or $result.arguments[1] -cne 'C:\fixture\') {throw 'Independent launch changed arguments'}
    $child=Start-Process -FilePath $ProbeExe -ArgumentList ('"'+$report+'" exit-37') -WindowStyle Hidden -PassThru
    if(-not $child.WaitForExit(15000)){throw 'Exit-status fixture did not exit normally'}
    if($child.ExitCode -ne 37){throw 'Independent launch lost nonzero child exit status'}
    "PASS: unpackaged fixture; reexecuted=$($result.reexecuted); Unicode/space/trailing-slash arguments and exit 37 preserved; no gateway/global calls."
} finally {
    $resolved=[IO.Path]::GetFullPath($sandbox)
    $tempRoot=[IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\')+'\'
    if(-not $resolved.StartsWith($tempRoot,[StringComparison]::OrdinalIgnoreCase) -or (Split-Path -Leaf $resolved) -notlike 'aec-identity-*'){throw 'Unsafe fixture cleanup'}
    Remove-Item -LiteralPath $resolved -Recurse -Force
}
