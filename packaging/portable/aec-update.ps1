param(
    [int]$CurrentPid = 0,
    [string]$CurrentVersion = "",
    [string]$DataDir = "",
    [switch]$UiOnly,
    [switch]$ResumeAgent,
    [switch]$OpenDashboard,
    [switch]$SelfTest
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

function Get-SafeRelativePath([string]$Path) {
    $relative = $Path.Replace('\', '/')
    if ([string]::IsNullOrWhiteSpace($relative) -or $relative.StartsWith('/') -or
        $relative.Contains(':') -or ($relative -split '/') -contains '..' -or
        ($relative -split '/') -contains '.') {
        throw "更新包包含不安全的路径"
    }
    return $relative.TrimEnd('/')
}

function Test-PackagePath([string]$Path) {
    $p = Get-SafeRelativePath $Path
    $instructionName = (-join [char[]]@(0x4F7F, 0x7528, 0x8BF4, 0x660E)) + '.txt'
    if ($p -match '^(aec-tray|aec-dev|aec-relay|aec-agent|aec-control)\.exe$' -or
        $p -eq $instructionName -or $p -eq 'app.ico' -or
        $p -eq 'ui/active.json' -or
        $p -match '^ui/revisions/[a-zA-Z0-9-]+/(setup\.html|help\.html|control\.html|dashboard\.html|agent\.html|theme\.css|app\.js|icon\.svg)$' -or
        $p -match '^(README\.md|LICENSE|THIRD-PARTY-NOTICES\.md|CHANGELOG\.md|CONTEXT\.md|CONTRIBUTING\.md|AGENTS\.md|SECURITY\.md|portable\.flag|SHA256SUMS\.txt|aec-update\.ps1)$' -or
        $p -match '^bin/aec-mihomo\.exe$' -or
        $p -match '^(docs|assets/readme|licenses)/[^/].*') {
        return $true
    }
    return $false
}

function Get-PackageFiles([string]$Root) {
    $rootPath = [IO.Path]::GetFullPath($Root).TrimEnd('\') + '\'
    $files = @()
    if (@(Get-ChildItem -LiteralPath $Root -Recurse -Force | Where-Object { $_.Attributes -band [IO.FileAttributes]::ReparsePoint }).Count) {
        throw '更新包不能包含链接'
    }
    Get-ChildItem -LiteralPath $Root -File -Recurse | ForEach-Object {
        $relative = $_.FullName.Substring($rootPath.Length).Replace('\', '/')
        if (-not (Test-PackagePath $relative)) {
            throw "更新包包含未允许的文件：$relative"
        }
        $files += [pscustomobject]@{ Relative = $relative; Source = $_.FullName }
    }
    return $files | Sort-Object Relative
}

function Assert-TargetPath([string]$TargetRoot, [string]$Relative) {
    $rootPath = [IO.Path]::GetFullPath($TargetRoot).TrimEnd('\')
    if (-not (Test-PackagePath $Relative)) { throw '目标文件不在允许列表' }
    $destination = [IO.Path]::GetFullPath((Join-Path $rootPath $Relative))
    if (-not $destination.StartsWith($rootPath + '\', [StringComparison]::OrdinalIgnoreCase)) { throw '目标路径越界' }
    $cursor = $destination
    while ($cursor -and $cursor.StartsWith($rootPath, [StringComparison]::OrdinalIgnoreCase)) {
        if ((Test-Path -LiteralPath $cursor) -and ((Get-Item -LiteralPath $cursor -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)) { throw '更新目标不能经过链接' }
        if ($cursor -eq $rootPath) { break }
        $cursor = Split-Path -Parent $cursor
    }
}

function Test-PackageIntegrity([string]$Root) {
    $files = @(Get-PackageFiles $Root | Where-Object { $_.Relative -ne 'SHA256SUMS.txt' })
    $sums = @{}
    foreach ($line in (Get-Content -LiteralPath (Join-Path $Root 'SHA256SUMS.txt'))) {
        if (-not $line.Trim()) { continue }
        if ($line -notmatch '^([a-fA-F0-9]{64})\s+\*?(.+)$') { throw '包内校验清单格式无效' }
        $hash = $Matches[1]; $relative = Get-SafeRelativePath $Matches[2]
        if (-not (Test-PackagePath $relative) -or $sums.ContainsKey($relative)) { throw '包内校验路径重复或非法' }
        $sums[$relative] = $hash
    }
    if ($sums.Count -ne $files.Count) { throw '包内校验清单不完整' }
    foreach ($file in $files) {
        if (-not $sums.ContainsKey($file.Relative) -or (Get-FileHash -LiteralPath $file.Source).Hash -ne $sums[$file.Relative]) { throw '包内文件校验失败' }
    }
}

function Prepare-PackageForTarget([string]$PackageRoot, [string]$TargetRoot) {
    if (-not (Test-Path -LiteralPath (Join-Path $TargetRoot 'portable.flag'))) {
        Remove-Item -LiteralPath (Join-Path $PackageRoot 'portable.flag') -Force -ErrorAction SilentlyContinue
    }
    return @(Get-PackageFiles $PackageRoot)
}

function Invoke-TransactionalCopy([string]$SourceRoot, [string]$TargetRoot, [string]$BackupRoot, $SelectedFiles = $null) {
    $targetFull = [IO.Path]::GetFullPath($TargetRoot).TrimEnd('\')
    $files = if ($null -eq $SelectedFiles) { @(Get-PackageFiles $SourceRoot) } else { @($SelectedFiles) }
    if ($files.Count -eq 0) { return }
    $originals = @{}
    $created = @()
    foreach ($file in $files) {
        Assert-TargetPath $TargetRoot $file.Relative
        $destination = [IO.Path]::GetFullPath((Join-Path $targetFull ($file.Relative.Replace('/', '\'))))
        if (-not $destination.StartsWith($targetFull + '\', [StringComparison]::OrdinalIgnoreCase) -or
            $destination -match '(?i)\\data\\') {
            throw "更新目标越界或触及 data 目录"
        }
        if (Test-Path -LiteralPath $destination -PathType Leaf) {
            $backup = Join-Path $BackupRoot ($file.Relative.Replace('/', '\'))
            New-Item -ItemType Directory -Path (Split-Path -Parent $backup) -Force | Out-Null
            [IO.File]::Copy($destination, $backup, $true)
            $originals[$destination] = $backup
        } else {
            $created += $destination
        }
    }
    try {
        foreach ($file in $files) {
            $destination = [IO.Path]::GetFullPath((Join-Path $targetFull ($file.Relative.Replace('/', '\'))))
            New-Item -ItemType Directory -Path (Split-Path -Parent $destination) -Force | Out-Null
            $temporary = "$destination.aec-update-new"
            [IO.File]::Copy($file.Source, $temporary, $true)
            Move-Item -LiteralPath $temporary -Destination $destination -Force
        }
    } catch {
        foreach ($destination in $created) {
            if (Test-Path -LiteralPath $destination -PathType Leaf) {
                Remove-Item -LiteralPath $destination -Force
            }
        }
        foreach ($destination in $originals.Keys) {
            [IO.File]::Copy($originals[$destination], $destination, $true)
        }
        throw
    }
}

function Get-UiManifest([string]$PackageRoot) {
    $manifestPath = Join-Path $PackageRoot 'ui/active.json'
    if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) { throw '更新包缺少独立界面资源；请选择完整更新' }
    $manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
    if ($manifest.schemaVersion -ne 2 -or $manifest.revision -notmatch '^[a-zA-Z0-9-]{1,80}$' -or
        @($manifest.PSObject.Properties.Name | Where-Object { $_ -notin @('schemaVersion','revision') }).Count) {
        throw '界面资源协议与当前版本不兼容，需要完整更新'
    }
    $revisionRoot = Join-Path $PackageRoot ('ui/revisions/' + $manifest.revision)
    foreach ($name in @('setup.html','help.html','control.html','dashboard.html','agent.html','theme.css','app.js','icon.svg')) {
        $file = Join-Path $revisionRoot $name
        if (-not (Test-Path -LiteralPath $file -PathType Leaf) -or (Get-Item -LiteralPath $file).Length -gt 2MB) {
            throw '界面资源不完整或超过大小限制'
        }
    }
    return $manifest
}

function Get-UpdatePlan([string]$PackageRoot, [string]$TargetRoot, [bool]$PresentationOnly) {
    $files = @(Get-PackageFiles $PackageRoot)
    if (-not ($files.Relative -contains 'aec-tray.exe')) { throw '完整更新包缺少托盘程序' }
    $changed = @($files | Where-Object {
        $destination = Join-Path $TargetRoot $_.Relative
        -not (Test-Path -LiteralPath $destination -PathType Leaf) -or
        (Get-FileHash -LiteralPath $_.Source).Hash -ne (Get-FileHash -LiteralPath $destination).Hash
    })
    if ($PresentationOnly) {
        [void](Get-UiManifest $PackageRoot)
        [void](Get-UiManifest $TargetRoot)
        $changed = @($changed | Where-Object { $_.Relative.StartsWith('ui/') })
    }
    foreach ($file in $changed) { Assert-TargetPath $TargetRoot $file.Relative }
    $core = @($changed | Where-Object {
        -not $_.Relative.StartsWith('ui/') -and -not $_.Relative.StartsWith('docs/') -and
        -not $_.Relative.StartsWith('assets/readme/') -and -not $_.Relative.StartsWith('licenses/') -and
        $_.Relative -notmatch '\.(md|txt)$' -and $_.Relative -notin @('LICENSE','SHA256SUMS.txt','app.ico')
    }).Count -gt 0
    [pscustomobject]@{ Files=$changed; CoreStopRequired=$core; UiOnly=$PresentationOnly }
}

function Invoke-UiSwitch([string]$PackageRoot, [string]$TargetRoot, [string]$BackupRoot) {
    $manifest = Get-UiManifest $PackageRoot
    Assert-TargetPath $TargetRoot 'ui/active.json'
    foreach ($name in @('setup.html','help.html','control.html','dashboard.html','agent.html','theme.css','app.js','icon.svg')) {
        Assert-TargetPath $TargetRoot ('ui/revisions/' + $manifest.revision + '/' + $name)
    }
    $ui = Join-Path $TargetRoot 'ui'
    if ((Get-Item -LiteralPath $ui).Attributes -band [IO.FileAttributes]::ReparsePoint) { throw '界面目标不能是链接目录' }
    $revisions = Join-Path $ui 'revisions'
    New-Item -ItemType Directory -Path $revisions -Force | Out-Null
    if ((Get-Item -LiteralPath $revisions).Attributes -band [IO.FileAttributes]::ReparsePoint) { throw '界面版本目录不能是链接目录' }
    $source = Join-Path $PackageRoot ('ui/revisions/' + $manifest.revision)
    $revision = Join-Path $revisions $manifest.revision
    $pending = Join-Path $revisions ('pending-' + [guid]::NewGuid().ToString('N'))
    $active = Join-Path $ui 'active.json'
    $pointer = Join-Path $ui ('active-' + [guid]::NewGuid().ToString('N') + '.pending')
    New-Item -ItemType Directory -Path $BackupRoot -Force | Out-Null
    $saved = Join-Path $BackupRoot 'ui-active.json'
    Copy-Item -LiteralPath $active -Destination $saved
    try {
        if (Test-Path -LiteralPath $revision) {
            if ((Get-Item -LiteralPath $revision).Attributes -band [IO.FileAttributes]::ReparsePoint) { throw '界面版本不能是链接目录' }
            foreach ($file in (Get-ChildItem -LiteralPath $source -File)) {
                if (-not (Test-Path -LiteralPath (Join-Path $revision $file.Name) -PathType Leaf) -or
                    (Get-FileHash -LiteralPath $file.FullName).Hash -ne (Get-FileHash -LiteralPath (Join-Path $revision $file.Name)).Hash) {
                    throw '相同界面版本已有不同内容，拒绝覆盖运行中的资源'
                }
            }
        } else {
            Copy-Item -LiteralPath $source -Destination $pending -Recurse
            foreach ($file in (Get-ChildItem -LiteralPath $source -File)) {
                if ((Get-FileHash -LiteralPath $file.FullName).Hash -ne (Get-FileHash -LiteralPath (Join-Path $pending $file.Name)).Hash) { throw '界面文件校验失败' }
            }
            Move-Item -LiteralPath $pending -Destination $revision
        }
        Copy-Item -LiteralPath (Join-Path $PackageRoot 'ui/active.json') -Destination $pointer
        [IO.File]::Replace($pointer, $active, (Join-Path $BackupRoot 'ui-replaced.json'))
    } catch {
        # The old pointer remains active until the single atomic Replace succeeds.
        throw
    } finally {
        if (Test-Path -LiteralPath $pending) { Remove-Item -LiteralPath $pending -Recurse -Force }
        if (Test-Path -LiteralPath $pointer) { Remove-Item -LiteralPath $pointer -Force }
    }
}

function Invoke-PlannedUpdate($Plan, [scriptblock]$ConfirmCore, [scriptblock]$StopCore, [scriptblock]$CopyFiles, [scriptblock]$RestartCore) {
    if ($Plan.CoreStopRequired) {
        if (-not (& $ConfirmCore)) { return 'cancelled' }
        $stopped = $false
        try {
            & $StopCore
            $stopped = $true
            & $CopyFiles
        } finally { & $RestartCore $stopped }
        return 'core-updated'
    }
    & $CopyFiles
    return 'hot-updated'
}

function Invoke-InstallPlan($Plan, [string]$Package, [string]$Install, [string]$Backup) {
    # Keep one rollback inventory for both document replacements and the UI pointer.
    $hotUi = -not $Plan.CoreStopRequired -and @($Plan.Files | Where-Object { $_.Relative.StartsWith('ui/') }).Count -gt 0
    $selected = @($Plan.Files | Where-Object { -not $hotUi -or -not $_.Relative.StartsWith('ui/') })
    $inventory = @($selected | ForEach-Object { [pscustomobject]@{ Relative=$_.Relative; Existed=(Test-Path -LiteralPath (Join-Path $Install $_.Relative) -PathType Leaf) } })
    New-Item -ItemType Directory -Path $Backup -Force | Out-Null
    ConvertTo-Json -InputObject $inventory -Depth 4 | Set-Content -LiteralPath (Join-Path $Backup 'rollback.json') -Encoding UTF8
    try {
        Invoke-TransactionalCopy $Package $Install $Backup $selected
        if ($hotUi) { Invoke-UiSwitch $Package $Install $Backup }
    } catch {
        Restore-UpdateBackup $Install $Backup
        throw
    }
}

function Restore-UpdateBackup([string]$Install, [string]$Backup) {
    $records = Get-Content -LiteralPath (Join-Path $Backup 'rollback.json') -Raw | ConvertFrom-Json
    foreach ($item in $records) {
        if ($null -eq $item) { continue }
        Assert-TargetPath $Install $item.Relative
        $destination = Join-Path $Install $item.Relative
        $saved = Join-Path $Backup $item.Relative
        if ($item.Existed) {
            if (Test-Path -LiteralPath $saved -PathType Leaf) { [IO.File]::Copy($saved, $destination, $true) }
        } elseif (Test-Path -LiteralPath $destination -PathType Leaf) { Remove-Item -LiteralPath $destination -Force }
    }
    $savedPointer = Join-Path $Backup 'ui-active.json'
    if (Test-Path -LiteralPath $savedPointer -PathType Leaf) {
        $active = Join-Path $Install 'ui/active.json'
        Assert-TargetPath $Install 'ui/active.json'
        $temporary = $active + '.rollback'
        [IO.File]::Copy($savedPointer, $temporary, $true)
        [IO.File]::Replace($temporary, $active, (Join-Path $Backup 'ui-rollback-replaced.json'))
    }
}

function Stop-OwnedApplication([string]$Install, [int]$TrayPid) {
    $tray = Join-Path $Install 'aec-tray.exe'
    $process = Get-Process -Id $TrayPid -ErrorAction Stop
    if ([IO.Path]::GetFullPath($process.Path) -ne [IO.Path]::GetFullPath($tray)) { throw '运行中的托盘身份不匹配，未停止服务' }
    Start-Process -FilePath $tray -ArgumentList '--quit' -WindowStyle Hidden | Out-Null
    $deadline = [DateTime]::UtcNow.AddSeconds(120)
    do {
        Start-Sleep -Milliseconds 300
        $running = -not $process.WaitForExit(1)
    } while ($running -and [DateTime]::UtcNow -lt $deadline)
    if ($running) { throw '托盘未能在 120 秒内正常退出；没有替换任何文件' }
    $data = if ($DataDir) { $DataDir } elseif (Test-Path -LiteralPath (Join-Path $Install 'portable.flag')) { Join-Path $Install 'data' } else { Join-Path $env:LOCALAPPDATA 'AgentEgressController' }
    $state = Get-Content -LiteralPath (Join-Path $data 'state.json') -Raw | ConvertFrom-Json
    if ($state.mode -ne 'daily' -or @($state.journal).Count -ne 0) { throw '系统恢复尚未完成；未替换程序，请重新启动托盘检查恢复状态' }
    if (@($state.unacknowledgedRestoreFailures).Count -ne 0) { throw '仍有未确认的恢复失败，未替换程序' }
    # Never close foreign processes. The tray must have stopped its own network children.
    $owned = @(Get-Process -Name aec-relay,aec-mihomo -ErrorAction SilentlyContinue | Where-Object {
        $_.Path -and [IO.Path]::GetFullPath($_.Path).StartsWith($Install.TrimEnd('\') + '\', [StringComparison]::OrdinalIgnoreCase)
    })
    if ($owned.Count) { throw '本应用内核或 relay 尚未正常停止；未替换文件，请从托盘检查恢复状态' }
    $controls = @(Get-Process -Name aec-control -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq (Join-Path $Install 'aec-control.exe') })
    foreach ($control in $controls) { [void]$control.CloseMainWindow() }
    foreach ($control in $controls) { if (-not $control.WaitForExit(10000)) { throw '本应用控制中心未正常关闭；未替换文件' } }
}

function Show-UpdateMessage([string]$Text, [string]$Title, [bool]$Confirm = $false) {
    Add-Type -AssemblyName System.Windows.Forms
    if ($Confirm) {
        return [Windows.Forms.MessageBox]::Show($Text, $Title, 'YesNo', 'Question') -eq 'Yes'
    }
    [void][Windows.Forms.MessageBox]::Show($Text, $Title, 'OK', 'Information')
    return $false
}

function Invoke-SelfTest {
    $temp = Join-Path ([IO.Path]::GetTempPath()) ("aec-update-test-" + [guid]::NewGuid().ToString('N'))
    $source = Join-Path $temp 'stage'
    $target = Join-Path $temp 'install'
    $backup = Join-Path $temp 'backup'
    New-Item -ItemType Directory -Path $source, $target, $backup | Out-Null
    try {
        $rejected = $false
        try { [void](Get-SafeRelativePath '../data/profiles.json') } catch { $rejected = $true }
        if (-not $rejected) { throw 'path traversal was accepted' }
        [IO.File]::WriteAllText((Join-Path $source 'aec-tray.exe'), 'new-tray')
        [IO.File]::WriteAllText((Join-Path $source 'portable.flag'), '')
        New-Item -ItemType Directory -Path (Join-Path $source 'data') | Out-Null
        [IO.File]::WriteAllText((Join-Path $source 'data/profiles.json'), 'must-not-install')
        $rejected = $false
        try { [void](Get-PackageFiles $source) } catch { $rejected = $true }
        if (-not $rejected) { throw 'data directory content was accepted' }
        Remove-Item -LiteralPath (Join-Path $source 'data') -Recurse -Force
        New-Item -ItemType Directory -Path (Join-Path $source 'docs') | Out-Null
        [IO.File]::WriteAllText((Join-Path $source 'docs/new.md'), 'new-doc')
        [IO.File]::WriteAllText((Join-Path $target 'aec-tray.exe'), 'old-tray')
        [IO.File]::WriteAllText((Join-Path $target 'portable.flag'), '')
        New-Item -ItemType Directory -Path (Join-Path $target 'data') | Out-Null
        [IO.File]::WriteAllText((Join-Path $target 'data/profiles.json'), 'keep-data')
        [void](Prepare-PackageForTarget $source $target)
        Invoke-TransactionalCopy $source $target $backup
        if ([IO.File]::ReadAllText((Join-Path $target 'aec-tray.exe')) -ne 'new-tray' -or
            [IO.File]::ReadAllText((Join-Path $target 'data/profiles.json')) -ne 'keep-data' -or
            -not (Test-Path -LiteralPath (Join-Path $target 'portable.flag'))) {
            throw 'update did not replace program files while preserving data'
        }
        $installedSource = Join-Path $temp 'installed-stage'
        $installedTarget = Join-Path $temp 'installed-app'
        $installedBackup = Join-Path $temp 'installed-backup'
        New-Item -ItemType Directory -Path $installedSource, $installedTarget, $installedBackup | Out-Null
        [IO.File]::WriteAllText((Join-Path $installedSource 'aec-tray.exe'), 'new-installed-tray')
        [IO.File]::WriteAllText((Join-Path $installedSource 'portable.flag'), '')
        [IO.File]::WriteAllText((Join-Path $installedTarget 'aec-tray.exe'), 'old-installed-tray')
        [void](Prepare-PackageForTarget $installedSource $installedTarget)
        Invoke-TransactionalCopy $installedSource $installedTarget $installedBackup
        if (Test-Path -LiteralPath (Join-Path $installedTarget 'portable.flag')) {
            throw 'installed update changed the installation to portable mode'
        }
        $failureSource = Join-Path $temp 'failure-stage'
        $failureTarget = Join-Path $temp 'failure-install'
        $failureBackup = Join-Path $temp 'failure-backup'
        New-Item -ItemType Directory -Path $failureSource, $failureTarget, $failureBackup | Out-Null
        New-Item -ItemType Directory -Path (Join-Path $failureSource 'docs') | Out-Null
        [IO.File]::WriteAllText((Join-Path $failureSource 'aec-tray.exe'), 'new-tray')
        [IO.File]::WriteAllText((Join-Path $failureSource 'docs/new.md'), 'new-doc')
        [IO.File]::WriteAllText((Join-Path $failureTarget 'aec-tray.exe'), 'old-tray')
        [IO.File]::WriteAllText((Join-Path $failureTarget 'docs'), 'block-doc-folder')
        $rejected = $false
        try { Invoke-TransactionalCopy $failureSource $failureTarget $failureBackup } catch { $rejected = $true }
        if (-not $rejected -or [IO.File]::ReadAllText((Join-Path $failureTarget 'aec-tray.exe')) -ne 'old-tray') {
            throw 'failed update did not roll back replaced files'
        }
        $hotSource = Join-Path $temp 'hot-source'
        $hotTarget = Join-Path $temp 'hot-target'
        New-Item -ItemType Directory -Path $hotSource,$hotTarget | Out-Null
        foreach ($entry in @(@($hotSource,'new'),@($hotTarget,'old'))) {
            $directory = Join-Path $entry[0] ('ui/revisions/' + $entry[1])
            New-Item -ItemType Directory -Path $directory -Force | Out-Null
            [IO.File]::WriteAllText((Join-Path $entry[0] 'ui/active.json'), ('{"schemaVersion":2,"revision":"' + $entry[1] + '"}'))
            foreach ($name in @('setup.html','help.html','control.html','dashboard.html','agent.html','theme.css','app.js','icon.svg')) { [IO.File]::WriteAllText((Join-Path $directory $name),$entry[1]) }
            [IO.File]::WriteAllText((Join-Path $entry[0] 'aec-tray.exe'),'same-core')
        }
        $hot = Get-UpdatePlan $hotSource $hotTarget $false
        if ($hot.CoreStopRequired -or $hot.Files.Count -ne 9) { throw 'UI changes were classified as core' }
        $script:events = @()
        $result = Invoke-PlannedUpdate $hot { throw 'hot update asked to stop' } { throw 'hot update stopped core' } { $script:events += 'copy' } { throw 'hot update restarted core' }
        if ($result -ne 'hot-updated' -or ($script:events -join ',') -ne 'copy') { throw 'hot update side effects incorrect' }
        [IO.File]::WriteAllText((Join-Path $hotSource 'aec-tray.exe'),'changed-core')
        $corePlan = Get-UpdatePlan $hotSource $hotTarget $false
        if (-not $corePlan.CoreStopRequired) { throw 'core change was classified as hot' }
        $script:events = @()
        $result = Invoke-PlannedUpdate $corePlan { $script:events += 'cancel'; $false } { throw 'cancelled update stopped core' } { throw 'cancelled update wrote files' } { throw 'cancelled update restarted' }
        if ($result -ne 'cancelled' -or ($script:events -join ',') -ne 'cancel') { throw 'cancel path incorrect' }
        $script:events = @()
        $result = Invoke-PlannedUpdate $corePlan { $script:events += 'confirm'; $true } { $script:events += 'stop' } { $script:events += 'copy' } { $script:events += 'restart' }
        if (($script:events -join ',') -ne 'confirm,stop,copy,restart') { throw 'core update must confirm before stopping' }
        $script:events = @()
        $stopFailed = $false
        try {
            Invoke-PlannedUpdate $corePlan { $true } { $script:events += 'stop'; throw 'restore incomplete' } { $script:events += 'copy' } { $script:events += 'recover' }
        } catch { $stopFailed = $true }
        if (-not $stopFailed -or ($script:events -join ',') -ne 'stop,recover') { throw 'stop failure must recover the old tray without copying files' }
        $onlyUi = Get-UpdatePlan $hotSource $hotTarget $true
        if ($onlyUi.CoreStopRequired -or $onlyUi.Files.Relative -contains 'aec-tray.exe') { throw 'UI-only update selected a binary' }
        [IO.File]::WriteAllText((Join-Path $hotTarget 'ui/active.json'), '{"schemaVersion":1,"revision":"old"}')
        $rejected = $false
        try { Get-UpdatePlan $hotSource $hotTarget $true | Out-Null } catch { $rejected = $true }
        if (-not $rejected) { throw 'incompatible UI protocol was accepted for hot update' }
        [IO.File]::WriteAllText((Join-Path $hotTarget 'ui/active.json'), '{"schemaVersion":2,"revision":"old"}')
        $hotBackup = Join-Path $temp 'hot-backup'
        Invoke-InstallPlan $onlyUi $hotSource $hotTarget $hotBackup
        if ((Get-UiManifest $hotTarget).revision -ne 'new' -or [IO.File]::ReadAllText((Join-Path $hotTarget 'aec-tray.exe')) -ne 'same-core') { throw 'hot pointer did not switch safely' }
        Restore-UpdateBackup $hotTarget $hotBackup
        if ((Get-UiManifest $hotTarget).revision -ne 'old') { throw 'hot rollback did not restore pointer' }
        [IO.File]::WriteAllText((Join-Path $hotTarget 'ui/revisions/new/app.js'),'conflicting-version')
        $rejected = $false
        try { Invoke-UiSwitch $hotSource $hotTarget (Join-Path $temp 'conflict-backup') } catch { $rejected = $true }
        if (-not $rejected -or (Get-UiManifest $hotTarget).revision -ne 'old') { throw 'failed staging changed the live pointer' }
        $instructionName = (-join [char[]]@(0x4F7F,0x7528,0x8BF4,0x660E)) + '.txt'
        if (-not (Test-PackagePath $instructionName)) { throw 'public instructions were rejected' }
        $hashLines = @(Get-PackageFiles $hotSource | ForEach-Object { (Get-FileHash -LiteralPath $_.Source).Hash + '  ' + $_.Relative })
        $hashLines | Set-Content -LiteralPath (Join-Path $hotSource 'SHA256SUMS.txt') -Encoding UTF8
        Test-PackageIntegrity $hotSource
        [IO.File]::WriteAllText((Join-Path $hotSource 'aec-tray.exe'),'tampered')
        $rejected = $false
        try { Test-PackageIntegrity $hotSource } catch { $rejected = $true }
        if (-not $rejected) { throw 'corrupt package was accepted' }
        Write-Output 'Update self-tests passed: core confirmation/cancel/order, UI-only with no stop, atomic pointer and rollback, immutable revisions, package integrity, traversal/data/install-mode preservation.'
    } finally {
        Remove-Item -LiteralPath $temp -Recurse -Force
    }
}

if ($SelfTest) {
    Invoke-SelfTest
    exit 0
}

try {
    if (-not $CurrentPid -or -not $CurrentVersion) { throw '请从托盘菜单启动更新检查' }
    $updateMutex = New-Object Threading.Mutex($false, 'Local\AgentEgressControllerUpdater')
    $ownsUpdateMutex = $updateMutex.WaitOne(0)
    if (-not $ownsUpdateMutex) { throw '另一项更新检查正在进行，请先完成或关闭该窗口' }
    $release = Invoke-RestMethod -Uri 'https://api.github.com/repos/zxs4KHf/agent-egress-controller/releases/latest' -Headers @{
        'User-Agent' = 'Agent-Egress-Controller-Updater'
        'Accept' = 'application/vnd.github+json'
    } -TimeoutSec 30
    $version = [string]$release.tag_name -replace '^v', ''
    if ($version -notmatch '^\d+\.\d+\.\d+$') { throw 'Release 版本格式不受支持' }
    if ([version]$version -le [version]$CurrentVersion -and -not $UiOnly) {
        Show-UpdateMessage "当前已是最新版本 v$CurrentVersion。" 'Agent Egress 更新'
        exit 0
    }
    $zipName = "AgentEgressController-$version-portable.zip"
    $sumName = "SHA256SUMS-$version.txt"
    $zipAsset = @($release.assets | Where-Object { $_.name -eq $zipName }) | Select-Object -First 1
    $sumAsset = @($release.assets | Where-Object { $_.name -eq $sumName }) | Select-Object -First 1
    if (-not $zipAsset -or -not $sumAsset) { throw 'Release 缺少便携包或 SHA256 校验文件' }
    if ([version]$version -lt [version]$CurrentVersion) { throw '不允许从旧 Release 更新界面' }
    $kind = if ($UiOnly) { '仅界面资源（不停止网络服务）' } else { '完整更新（如核心改变，安装前会再次询问停服）' }
    $body = "Release v$version（核心当前 v$CurrentVersion）。$kind。将下载完整便携包、核验 SHA256 后比较实际改动，保留用户数据。现在下载？"
    if (-not (Show-UpdateMessage $body 'Agent Egress 更新' $true)) { exit 0 }
    $temp = Join-Path ([IO.Path]::GetTempPath()) ("aec-update-" + [guid]::NewGuid().ToString('N'))
    $download = Join-Path $temp $zipName
    $sums = Join-Path $temp $sumName
    $stage = Join-Path $temp 'expanded'
    New-Item -ItemType Directory -Path $temp, $stage | Out-Null
    try {
        Invoke-WebRequest -Uri $zipAsset.browser_download_url -OutFile $download -TimeoutSec 180
        Invoke-WebRequest -Uri $sumAsset.browser_download_url -OutFile $sums -TimeoutSec 30
        $sumText = Get-Content -LiteralPath $sums -Raw
        $sumMatch = [regex]::Match($sumText, '(?im)^([a-f0-9]{64})\s+\*?' + [regex]::Escape($zipName) + '\s*$')
        if (-not $sumMatch.Success) { throw '校验文件中没有对应 ZIP 的 SHA256' }
        $actualHash = (Get-FileHash -LiteralPath $download -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($actualHash -ne $sumMatch.Groups[1].Value.ToLowerInvariant()) { throw 'SHA256 校验失败，未安装更新' }
        if ((Get-Item -LiteralPath $download).Length -gt 500MB) { throw '更新包超过大小限制' }
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        $archive = [IO.Compression.ZipFile]::OpenRead($download)
        try {
            if ($archive.Entries.Count -gt 10000) { throw '更新包文件数量超过限制' }
            foreach ($entry in $archive.Entries) {
                [void](Get-SafeRelativePath $entry.FullName)
                if ($entry.Length -gt 250MB) { throw '更新包中存在超大文件' }
            }
        } finally { $archive.Dispose() }
        Expand-Archive -LiteralPath $download -DestinationPath $stage -Force
        $roots = @(Get-ChildItem -LiteralPath $stage -Directory)
        if ($roots.Count -ne 1) { throw '更新包目录结构无效' }
        $install = [IO.Path]::GetFullPath($PSScriptRoot)
        $expectedDir = "AgentEgressController-$version-portable"
        if ($roots[0].Name -ne $expectedDir) { throw '更新包目录版本与 Release 标签不一致' }
        $package = $roots[0].FullName
        $tray = Join-Path $install 'aec-tray.exe'
        if (-not (Test-Path -LiteralPath $tray -PathType Leaf)) { throw '当前目录不像 Agent Egress 便携安装目录' }
        Test-PackageIntegrity $package
        [void](Prepare-PackageForTarget $package $install)
        $plan = Get-UpdatePlan $package $install $UiOnly.IsPresent
        if ($plan.Files.Count -eq 0) {
            Show-UpdateMessage '所选更新内容已经一致，无需安装。' 'Agent Egress 更新'
            exit 0
        }
        $backup = Join-Path $install ('.aec-update-backup/' + [DateTime]::UtcNow.ToString('yyyyMMdd-HHmmss') + '-' + [guid]::NewGuid().ToString('N'))
        $confirm = {
            Show-UpdateMessage "本次更新包含核心程序，将正常关闭本应用的网络服务和控制中心，再替换程序并重启。连接会短暂中断；固定 ISP 配置、用户数据和凭据保留，不会改为其他出口。是否现在停服更新？" '核心更新需要确认' $true
        }
        $stop = { Stop-OwnedApplication $install $CurrentPid }
        $copy = { Invoke-InstallPlan $plan $package $install $backup }
        $restart = {
            param([bool]$StopCompleted)
            $stillRunning = Get-Process -Id $CurrentPid -ErrorAction SilentlyContinue
            if ($stillRunning -and $stillRunning.Path -eq $tray) { return }
            $arguments = @()
            # A failed restoration opens the old tray for recovery, never automatically re-enters.
            if ($ResumeAgent -and $StopCompleted) { $arguments += '--enter' }
            if ($OpenDashboard) { $arguments += '--dashboard' }
            $options = @{ FilePath=(Join-Path $install 'aec-tray.exe'); WorkingDirectory=$install; WindowStyle='Hidden' }
            if ($arguments.Count) { $options.ArgumentList=$arguments }
            try {
                $started = Start-Process @options -PassThru
                Start-Sleep -Seconds 2
                if ($started.HasExited) { throw '新版托盘启动后立即退出' }
            } catch {
                if (Test-Path -LiteralPath (Join-Path $backup 'rollback.json')) { Restore-UpdateBackup $install $backup }
                Start-Process @options | Out-Null
                throw '新程序未启动，已恢复旧程序并尝试启动；请检查托盘和恢复状态'
            }
        }
        $result = Invoke-PlannedUpdate $plan $confirm $stop $copy $restart
        if ($result -eq 'cancelled') {
            Show-UpdateMessage '已取消核心更新，服务和程序文件保持原样。' 'Agent Egress 更新'
        } elseif ($result -eq 'hot-updated') {
            Show-UpdateMessage "界面/文档已在线更新，网络服务未停止。请在控制中心点击刷新界面，未保存草稿会先询问。核心程序版本没有变化。`n回滚备份：$backup" 'Agent Egress 更新'
        } else {
            Show-UpdateMessage "程序已更新到 v$version，并已启动托盘。是否就绪以重新检测结果为准。`n回滚备份：$backup" 'Agent Egress 更新'
        }
    } catch {
        throw
    } finally {
        Remove-Item -LiteralPath $temp -Recurse -Force -ErrorAction SilentlyContinue
    }
} catch {
    if (-not $SelfTest) { Show-UpdateMessage $_.Exception.Message 'Agent Egress 更新失败' }
    exit 1
} finally {
    if ($ownsUpdateMutex) { $updateMutex.ReleaseMutex() }
    if ($updateMutex) { $updateMutex.Dispose() }
}
