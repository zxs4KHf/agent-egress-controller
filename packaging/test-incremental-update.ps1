param([string]$AgentExe = (Join-Path (Split-Path -Parent $PSScriptRoot) 'target/debug/aec-agent.exe'))
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
function Import-Functions([string]$Path) {
    $tokens=$null; $errors=$null
    $ast=[Management.Automation.Language.Parser]::ParseFile($Path,[ref]$tokens,[ref]$errors)
    if ($errors.Count) { throw 'Updater parse error' }
    [scriptblock]::Create((@($ast.EndBlock.Statements | Where-Object { $_ -is [Management.Automation.Language.FunctionDefinitionAst] } | ForEach-Object { $_.Extent.Text }) -join "`n"))
}
. (Import-Functions (Join-Path $repo 'packaging/portable/aec-update.ps1'))
# These must exercise the same planner/downloader/journal used by the interactive updater.
foreach ($name in @('Get-IncrementalPlan','Receive-IncrementalPackage','Initialize-UpdateLog','Write-UpdateFailure')) {
    if (-not (Get-Command $name -ErrorAction SilentlyContinue)) { throw "REGRESSION: $name missing; downloads are full ZIP and failures are popup-only" }
}
$sandbox=Join-Path ([IO.Path]::GetTempPath()) ('aec-incremental-test-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $sandbox | Out-Null
function Assert($Condition,[string]$Message) { if (-not $Condition) { throw $Message } }
function Reject([scriptblock]$Action,[string]$Message) { $failed=$false; try { & $Action | Out-Null } catch { $failed=$true }; Assert $failed $Message }
try {
    $package=Join-Path $sandbox 'source'; $target=Join-Path $sandbox 'target'; $assets=Join-Path $sandbox 'assets'
    New-Item -ItemType Directory -Path (Join-Path $package 'bin'),(Join-Path $package 'docs'),$target | Out-Null
    $random=[Random]::new(42); $bytes=New-Object byte[] 1048576; $random.NextBytes($bytes)
    [IO.File]::WriteAllBytes((Join-Path $package 'bin/aec-mihomo.exe'),$bytes)
    [IO.File]::WriteAllText((Join-Path $package 'aec-tray.exe'),'same-synthetic-tray')
    [IO.File]::WriteAllText((Join-Path $package 'docs/test.md'),'new-doc')
    [IO.File]::WriteAllText((Join-Path $package 'portable.flag'),'')
    $ui=Join-Path $package 'ui/revisions/test-revision'
    New-Item -ItemType Directory -Path $ui -Force | Out-Null
    [IO.File]::WriteAllText((Join-Path $package 'ui/active.json'),'{"schemaVersion":2,"revision":"test-revision"}')
    foreach ($name in @('setup.html','help.html','control.html','dashboard.html','agent.html','theme.css','app.js','icon.svg')) { [IO.File]::WriteAllText((Join-Path $ui $name),'new-ui') }
    @(Get-PackageFiles $package | ForEach-Object { (Get-FileHash -LiteralPath $_.Source).Hash + '  ' + $_.Relative }) | Set-Content -LiteralPath (Join-Path $package 'SHA256SUMS.txt') -Encoding UTF8
    Copy-Item -Path (Join-Path $package '*') -Destination $target -Recurse
    [IO.File]::WriteAllText((Join-Path $target 'docs/test.md'),'old-doc')
    New-Item -ItemType Directory -Path (Join-Path $target 'data') | Out-Null
    [IO.File]::WriteAllText((Join-Path $target 'data/profiles.json'),'private-fixture-must-stay')
    & (Join-Path $PSScriptRoot 'build-incremental.ps1') -Package $package -Version '9.0.0' -Output $assets
    $manifest=Get-Content -LiteralPath (Join-Path $assets 'aec-incremental-9.0.0.json') -Raw | ConvertFrom-Json
    $plan=Get-IncrementalPlan $manifest $target $false
    Assert ($plan.Components.Count -eq 1 -and $plan.Components[0].id -eq 'resources') 'Unchanged core must not be downloaded'
    Assert ($plan.DownloadBytes -lt 10000) 'Incremental payload did not avoid the 1 MB unchanged core'
    Initialize-UpdateLog (Join-Path $sandbox 'data') '8.0.0' $false
    $script:UpdateLog.transferMode='incremental'; $script:UpdateLog.plannedBytes=$plan.DownloadBytes
    $script:UpdateLog.skippedFiles=$plan.Unchanged.Count
    # Real loopback HTTP transfer of the actual component ZIP, no production proxy or service.
    $listener=[Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback,0); $listener.Start()
    $port=$listener.LocalEndpoint.Port
    $server=[PowerShell]::Create()
    $serveOnce={ param($listener,$file,[int]$status=200)
        $client=$listener.AcceptTcpClient()
        try {
            $stream=$client.GetStream(); $stream.ReadTimeout=5000
            $reader=[IO.StreamReader]::new($stream)
            while ($reader.ReadLine()) { }
            $body=[IO.File]::ReadAllBytes($file)
            $header=[Text.Encoding]::ASCII.GetBytes("HTTP/1.1 $status Test`r`nContent-Length: $($body.Length)`r`nConnection: close`r`n`r`n")
            $stream.Write($header,0,$header.Length); $stream.Write($body,0,$body.Length)
            return $body.Length
        } finally { $client.Dispose() }
    }
    [void]$server.AddScript($serveOnce).AddArgument($listener).AddArgument((Join-Path $assets $plan.Components[0].asset))
    $pending=$server.BeginInvoke()
    try {
        $stage=Receive-IncrementalPackage $manifest $plan $target (Join-Path $sandbox 'work') {
            param($c,$destination)
            Receive-UpdateAsset ([pscustomobject]@{size=$c.size; browser_download_url="http://127.0.0.1:$port/$($c.asset)"}) $destination
        }
        $sent=@($server.EndInvoke($pending))[0]
        Assert ($sent -eq $plan.DownloadBytes -and $script:UpdateLog.downloadedBytes -eq $sent) 'Real HTTP transfer byte count incorrect'
    } finally { $listener.Stop(); $server.Dispose() }
    Test-PackageIntegrity $stage
    $installPlan=Get-UpdatePlan $stage $target $false
    Assert (-not $installPlan.CoreStopRequired) 'Docs-only update must be hot'
    $backup=Join-Path $sandbox 'backup'
    [void](Invoke-PlannedUpdate $installPlan { throw 'confirmation unexpectedly required' } { throw 'service stopped' } { Invoke-InstallPlan $installPlan $stage $target $backup } { throw 'service restarted' })
    Assert ((Get-IncrementalPlan $manifest $target $false).DownloadBytes -eq 0) 'Repeat requires payload download'
    Restore-UpdateBackup $target $backup
    Assert ([IO.File]::ReadAllText((Join-Path $target 'docs/test.md')) -eq 'old-doc') 'Rollback failed'
    Assert ([IO.File]::ReadAllText((Join-Path $target 'data/profiles.json')) -eq 'private-fixture-must-stay') 'User data changed'
    Write-Output "PASS: actual HTTP incremental bytes=$sent; unchanged 1048576-byte core skipped; verified reconstruction, hot install, rollback, no-op and data preservation."

    # UI-only does not fetch a changed executable, nor select it for installation.
    [IO.File]::WriteAllText((Join-Path $target 'aec-tray.exe'),'older-tray')
    Remove-Item -LiteralPath (Join-Path $target 'ui/revisions/test-revision/app.js')
    $uiPlan=Get-IncrementalPlan $manifest $target $true
    Assert ($uiPlan.Components.Count -eq 1 -and $uiPlan.Components[0].id -eq 'ui') 'UI-only downloaded core'
    $uiStage=Receive-IncrementalPackage $manifest $uiPlan $target (Join-Path $sandbox 'ui-work') { param($c,$d) Copy-Item -LiteralPath (Join-Path $assets $c.asset) -Destination $d }
    # Restore the missing local file for protocol validation after download.
    Copy-Item -LiteralPath (Join-Path $package 'ui/revisions/test-revision/app.js') -Destination (Join-Path $target 'ui/revisions/test-revision/app.js')
    Assert (-not (Get-UpdatePlan $uiStage $target $true).CoreStopRequired) 'Partial UI stage classified as core'
    $corePlan=Get-UpdatePlan $stage $target $false
    Assert $corePlan.CoreStopRequired 'Changed executable not classified as core'
    Assert ((Invoke-PlannedUpdate $corePlan { $false } { throw 'Stopped after cancel' } { throw 'Copied after cancel' } { throw 'Restarted after cancel' }) -eq 'cancelled') 'Cancel failed'

    $bad=$manifest | ConvertTo-Json -Depth 8 | ConvertFrom-Json
    $bad.files[0].path='../data/credentials'
    Reject { Assert-IncrementalManifest $bad '9.0.0' } 'Traversal accepted'
    $bad=$manifest | ConvertTo-Json -Depth 8 | ConvertFrom-Json
    $bad.files += $bad.files[0]
    Reject { Assert-IncrementalManifest $bad '9.0.0' } 'Duplicate accepted'
    $bad=$manifest | ConvertTo-Json -Depth 8 | ConvertFrom-Json
    $bad.components[0].asset='https://example.invalid/evil.zip'
    Reject { Assert-IncrementalManifest $bad '9.0.0' } 'Foreign component URL accepted'
    $bad=$manifest | ConvertTo-Json -Depth 8 | ConvertFrom-Json
    ($bad.files | Where-Object { $_.path -eq 'ui/active.json' }).component='core'
    Reject { Assert-IncrementalManifest $bad '9.0.0' } 'Mixed UI/core component accepted'
    Reject { Receive-IncrementalPackage $manifest $plan $target (Join-Path $sandbox 'tamper-work') { param($c,$d) [IO.File]::WriteAllText($d,'tampered') } } 'Corrupt payload accepted'

    # The exact observability regression: exception gone and dialog closed, evidence remains on disk.
    Initialize-UpdateLog (Join-Path $sandbox 'data') '8.0.0' $false
    $secret='password=NEVER-PERSIST token=NEVER-PERSIST https://user:NEVER-PERSIST@example.invalid'
    try { Invoke-UpdateStep 'download_components' { throw [Net.WebException]::new($secret,[Net.WebExceptionStatus]::Timeout) } } catch { }
    $logPath=Join-Path $script:UpdateLogRoot ($script:UpdateLog.id + '.json')
    $text=[IO.File]::ReadAllText($logPath); $record=$text | ConvertFrom-Json
    Assert ($record.events[-1].status -eq 'failed' -and $record.events[-1].stage -eq 'download_components' -and $record.events[-1].code -eq 'network_timeout') 'Failure reason not persisted'
    Assert (-not $text.Contains('NEVER-PERSIST') -and -not $text.Contains('example.invalid')) 'Log leaked exception secrets'
    if (-not (Test-Path -LiteralPath $AgentExe -PathType Leaf)) { throw 'Build aec-agent before this test' }
    $before=(Get-FileHash -LiteralPath $logPath).Hash
    $agentOutput=& $AgentExe --data-dir (Join-Path $sandbox 'data') update-status
    Assert ($LASTEXITCODE -eq 0) 'Offline Agent command failed'
    $agentResult=($agentOutput -join "`n") | ConvertFrom-Json
    Assert ($agentResult.sessions[0].events[-1].code -eq 'network_timeout') 'Agent cannot read persistent failure without running tray'
    Assert ((Get-FileHash -LiteralPath $logPath).Hash -eq $before) 'Read-only Agent changed log'
    Assert (-not ($agentOutput -join '').Contains('NEVER-PERSIST')) 'Agent output leaked exception'
    $listener=[Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback,0); $listener.Start()
    $port=$listener.LocalEndpoint.Port; $server=[PowerShell]::Create()
    [void]$server.AddScript($serveOnce).AddArgument($listener).AddArgument($logPath).AddArgument(503)
    $pending=$server.BeginInvoke()
    try {
        Reject { Invoke-UpdateStep 'download_components' {
            Receive-UpdateAsset ([pscustomobject]@{size=1;browser_download_url="http://127.0.0.1:$port/fail"}) (Join-Path $sandbox 'failed-download')
        } } 'HTTP 503 was accepted'
        [void]$server.EndInvoke($pending)
        $record=Get-Content -LiteralPath $logPath -Raw | ConvertFrom-Json
        Assert ($record.events[-1].code -eq 'http_failed' -and $record.events[-1].httpStatus -eq 503) 'Actual HTTP error status was not recorded'
    } finally { $listener.Stop(); $server.Dispose() }
    # Run the actual interactive entry flow in a child PowerShell, but replace only external
    # I/O/dialog/lifecycle boundaries. A unique mutex and synthetic install make it isolated.
    $updaterPath=Join-Path $repo 'packaging/portable/aec-update.ps1'
    $tokens=$null; $parseErrors=$null
    $ast=[Management.Automation.Language.Parser]::ParseFile($updaterPath,[ref]$tokens,[ref]$parseErrors)
    $definitions=(@($ast.EndBlock.Statements | Where-Object { $_ -is [Management.Automation.Language.FunctionDefinitionAst] } | ForEach-Object { $_.Extent.Text }) -join "`n")
    $entry=(@($ast.EndBlock.Statements | Where-Object { $_ -isnot [Management.Automation.Language.FunctionDefinitionAst] } | ForEach-Object { $_.Extent.Text }) -join "`n")
    $entry=$entry.Replace('Local\AgentEgressControllerUpdater',('Local\AEC-Test-' + [guid]::NewGuid().ToString('N')))
    $release=[pscustomobject]@{tag_name='v9.0.0'; assets=@(Get-ChildItem -LiteralPath $assets -File | ForEach-Object {
        [pscustomobject]@{name=$_.Name;size=$_.Length;browser_download_url=('https://github.com/zxs4KHf/agent-egress-controller/releases/download/v9.0.0/' + $_.Name)}
    }) + @(
        [pscustomobject]@{name='AgentEgressController-9.0.0-portable.zip';size=1048576;browser_download_url='https://github.com/zxs4KHf/agent-egress-controller/releases/download/v9.0.0/AgentEgressController-9.0.0-portable.zip'},
        [pscustomobject]@{name='SHA256SUMS-9.0.0.txt';size=100;browser_download_url='https://github.com/zxs4KHf/agent-egress-controller/releases/download/v9.0.0/SHA256SUMS-9.0.0.txt'}
    )}
    $releaseFile=Join-Path $assets 'release-fixture.json'
    [IO.File]::WriteAllText($releaseFile,($release | ConvertTo-Json -Depth 8))
    $overrides=@'
function Invoke-RestMethod { param($Uri,$Headers,$TimeoutSec)
    if ($Scenario -eq 'discovery-fails') { throw [Net.WebException]::new('password=SECRET',[Net.WebExceptionStatus]::Timeout) }
    Get-Content -LiteralPath (Join-Path $Assets 'release-fixture.json') -Raw | ConvertFrom-Json
}
function Receive-UpdateAsset($Asset,[string]$Destination) {
    if ($Scenario -eq 'manifest-fails' -and $Asset.name -like 'aec-incremental*') { Stop-Update 'hash_mismatch' }
    Copy-Item -LiteralPath (Join-Path $Assets $Asset.name) -Destination $Destination
    $script:UpdateLog.downloadedBytes += (Get-Item -LiteralPath $Destination).Length
}
function Show-UpdateMessage([string]$Text,[string]$Title,[bool]$Confirm=$false) {
    if (-not $Confirm) { return $false }
    if ($script:UpdateStage -eq 'confirm_core') { return $false }
    return $Scenario -ne 'cancel-download'
}
function Stop-OwnedApplication { throw 'TEST MUST NEVER STOP A REAL SERVICE' }
function Start-Process { throw 'TEST MUST NEVER START A REAL SERVICE' }
'@
    foreach ($case in @(
        @{Name='discovery-fails';Status='failed';Code='network_timeout'},
        @{Name='manifest-fails';Status='failed';Code='hash_mismatch'},
        @{Name='cancel-download';Status='cancelled';Code='none'},
        @{Name='cancel-core';Status='cancelled';Code='none'},
        @{Name='hot-install';Status='succeeded';Code='hot_updated'}
    )) {
        $install=Join-Path $sandbox ('entry-' + $case.Name)
        New-Item -ItemType Directory -Path $install | Out-Null
        Copy-Item -Path (Join-Path $package '*') -Destination $install -Recurse
        [IO.File]::WriteAllText((Join-Path $install 'docs/test.md'),'old-doc')
        if ($case.Name -eq 'cancel-core') { [IO.File]::WriteAllText((Join-Path $install 'aec-tray.exe'),'old-tray') }
        $runner=Join-Path $install 'test-entry.ps1'
        $prefix='param([string]$Assets,[string]$Scenario)' + "`n" + '$CurrentPid=1; $CurrentVersion="8.0.0"; $DataDir=Join-Path $PSScriptRoot "data"; [switch]$UiOnly=$false; $SelfTest=$false; $ResumeAgent=$false; $OpenDashboard=$false' + "`n"
        [IO.File]::WriteAllText($runner,($prefix + $definitions + "`n" + $overrides + "`n" + $entry),[Text.UTF8Encoding]::new($true))
        & powershell -NoProfile -ExecutionPolicy Bypass -File $runner -Assets $assets -Scenario $case.Name | Out-Null
        $expectedExit=if ($case.Status -eq 'failed') {1} else {0}
        Assert ($LASTEXITCODE -eq $expectedExit) "Entry flow wrong exit: $($case.Name)"
        $log=Get-ChildItem -LiteralPath (Join-Path $install 'data/update-logs') -Filter '*.json' | Select-Object -First 1
        $run=Get-Content -LiteralPath $log.FullName -Raw | ConvertFrom-Json
        Assert ($run.events[-1].status -eq $case.Status -and $run.events[-1].code -eq $case.Code) "Entry flow lost terminal evidence: $($case.Name)"
        Assert (-not (Get-Content -LiteralPath $log.FullName -Raw).Contains('SECRET')) 'Entry flow secret leaked'
        $expectedDoc=if ($case.Name -eq 'hot-install') {'new-doc'} else {'old-doc'}
        Assert ([IO.File]::ReadAllText((Join-Path $install 'docs/test.md')) -eq $expectedDoc) "Entry flow changed files after failure/cancel: $($case.Name)"
        Write-Output "PASS: exact updater entry flow $($case.Name), persisted terminal result; no real lifecycle calls."
    }
    # Even if logging becomes unwritable after shutdown, recovery still runs.
    $script:UpdateLogRoot=Join-Path $sandbox 'blocks-log-directory'
    [IO.File]::WriteAllText($script:UpdateLogRoot,'file')
    $script:recovered=$false
    Reject { Invoke-UpdateStep 'restart' { $script:recovered=$true } } 'Log failure was not reported'
    Assert $script:recovered 'Log failure prevented recovery'
    $script:UpdateLog=$null
    Write-Output 'PASS: offline persistent failure stage/code after dismissal; exception secrets excluded; corrupt manifest/payload rejected; UI-only/cancel do not stop service.'
} finally {
    $resolved=[IO.Path]::GetFullPath($sandbox)
    $tempRoot=[IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\') + '\'
    if (-not $resolved.StartsWith($tempRoot,[StringComparison]::OrdinalIgnoreCase) -or (Split-Path -Leaf $resolved) -notlike 'aec-incremental-test-*') { throw 'Unsafe cleanup' }
    Remove-Item -LiteralPath $resolved -Recurse -Force
}
