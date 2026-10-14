param(
    [int]$CurrentPid = 0,
    [string]$CurrentVersion = "",
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
    if ($p -match '^(aec-tray|aec-dev|aec-relay|aec-agent|aec-control)\.exe$' -or
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
    Get-ChildItem -LiteralPath $Root -File -Recurse | ForEach-Object {
        $relative = $_.FullName.Substring($rootPath.Length).Replace('\', '/')
        if (-not (Test-PackagePath $relative)) {
            throw "更新包包含未允许的文件：$relative"
        }
        $files += [pscustomobject]@{ Relative = $relative; Source = $_.FullName }
    }
    return $files | Sort-Object Relative
}

function Prepare-PackageForTarget([string]$PackageRoot, [string]$TargetRoot) {
    if (-not (Test-Path -LiteralPath (Join-Path $TargetRoot 'portable.flag'))) {
        Remove-Item -LiteralPath (Join-Path $PackageRoot 'portable.flag') -Force -ErrorAction SilentlyContinue
    }
    return @(Get-PackageFiles $PackageRoot)
}

function Invoke-TransactionalCopy([string]$SourceRoot, [string]$TargetRoot, [string]$BackupRoot) {
    $targetFull = [IO.Path]::GetFullPath($TargetRoot).TrimEnd('\')
    $files = @(Get-PackageFiles $SourceRoot)
    if ($files.Count -eq 0 -or -not ($files.Relative -contains 'aec-tray.exe')) {
        throw "更新包缺少托盘程序"
    }
    $originals = @{}
    $created = @()
    foreach ($file in $files) {
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
        Write-Output 'Update self-tests passed: traversal rejected, data preserved, rollback restored.'
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
    $release = Invoke-RestMethod -Uri 'https://api.github.com/repos/zxs4KHf/agent-egress-controller/releases/latest' -Headers @{
        'User-Agent' = 'Agent-Egress-Controller-Updater'
        'Accept' = 'application/vnd.github+json'
    } -TimeoutSec 30
    $version = [string]$release.tag_name -replace '^v', ''
    if ($version -notmatch '^\d+\.\d+\.\d+$') { throw 'Release 版本格式不受支持' }
    if ([version]$version -le [version]$CurrentVersion) {
        Show-UpdateMessage "当前已是最新版本 v$CurrentVersion。" 'Agent Egress 更新'
        exit 0
    }
    $zipName = "AgentEgressController-$version-portable.zip"
    $sumName = "SHA256SUMS-$version.txt"
    $zipAsset = @($release.assets | Where-Object { $_.name -eq $zipName }) | Select-Object -First 1
    $sumAsset = @($release.assets | Where-Object { $_.name -eq $sumName }) | Select-Object -First 1
    if (-not $zipAsset -or -not $sumAsset) { throw 'Release 缺少便携包或 SHA256 校验文件' }
    $body = "发现新版本 v$version（当前 v$CurrentVersion）。将下载完整便携包，在本目录原位更新并保留 data。需要正常退出托盘，可能短暂中断本机代理。现在下载并安装？"
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
        [void](Prepare-PackageForTarget $package $install)
        $quit = Start-Process -FilePath $tray -ArgumentList '--quit' -PassThru -WindowStyle Hidden
        $deadline = [DateTime]::UtcNow.AddSeconds(120)
        do {
            Start-Sleep -Milliseconds 500
            $running = Get-Process -Id $CurrentPid -ErrorAction SilentlyContinue
        } while ($running -and [DateTime]::UtcNow -lt $deadline)
        if ($running) { throw '托盘未能在 120 秒内正常退出；没有替换任何文件' }
        $backup = Join-Path $install ".aec-update-backup\from-$CurrentVersion-to-$version"
        if (Test-Path -LiteralPath $backup) { throw '该版本的更新备份已存在，请先移走或改名后再更新' }
        New-Item -ItemType Directory -Path $backup | Out-Null
        Invoke-TransactionalCopy $package $install $backup
        Start-Process -FilePath (Join-Path $install 'aec-tray.exe') -WorkingDirectory $install
        Show-UpdateMessage "已更新到 v$version。用户数据与凭据未被更新器改动。`n回滚备份位于：$backup" 'Agent Egress 更新'
    } catch {
        throw
    } finally {
        Remove-Item -LiteralPath $temp -Recurse -Force -ErrorAction SilentlyContinue
    }
} catch {
    if (-not $SelfTest) { Show-UpdateMessage $_.Exception.Message 'Agent Egress 更新失败' }
    exit 1
}
