param([string]$Dist = (Join-Path (Split-Path -Parent $PSScriptRoot) 'dist'))
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

# Never execute the updater's interactive entry point or stop/start real services.
# Extract only top-level functions from pinned repository revisions, then exercise
# the actual copy/planning/rollback functions against the actual release ZIP.
function Get-UpdaterFunctions([string]$Source) {
    $tokens = $null; $errors = $null
    $ast = [Management.Automation.Language.Parser]::ParseInput($Source, [ref]$tokens, [ref]$errors)
    if ($errors.Count) { throw 'Updater source does not parse' }
    $functions = @($ast.EndBlock.Statements | Where-Object { $_ -is [Management.Automation.Language.FunctionDefinitionAst] })
    if (-not $functions.Count) { throw 'No updater functions found' }
    return [scriptblock]::Create(($functions | ForEach-Object { $_.Extent.Text }) -join "`n")
}

function Assert-ReleaseHash([string]$Asset, [string]$Sidecar) {
    $name = Split-Path -Leaf $Asset
    $match = [regex]::Match((Get-Content -LiteralPath $Sidecar -Raw), '(?im)^([a-f0-9]{64})\s+\*?' + [regex]::Escape($name) + '\s*$')
    if (-not $match.Success -or (Get-FileHash -LiteralPath $Asset).Hash -ne $match.Groups[1].Value) {
        throw "Release SHA256 mismatch: $name"
    }
}

$repo = Split-Path -Parent $PSScriptRoot
$version = [regex]::Match((Get-Content (Join-Path $repo 'Cargo.toml') -Raw), 'version = "([0-9.]+)"').Groups[1].Value
if ([version]$version -le [version]'0.5.1') { throw 'Compatibility test requires a newer release' }
$zip = Join-Path $Dist "AgentEgressController-$version-portable.zip"
$bootstrap = Join-Path $Dist "aec-update-$version.ps1"
Assert-ReleaseHash $zip (Join-Path $Dist "SHA256SUMS-$version.txt")
Assert-ReleaseHash $bootstrap (Join-Path $Dist "SHA256UPDATER-$version.txt")
$setup = Join-Path $Dist "AgentEgressController-$version-setup.exe"
if (Test-Path -LiteralPath $setup) { Assert-ReleaseHash $setup (Join-Path $Dist "SHA256SETUP-$version.txt") }

$sources = @{}
Push-Location $repo
try {
    foreach ($entry in @(
        @{ Version='0.5.0'; Commit='d946400a93022c0dd53468c53398900c2ab25ca0' },
        @{ Version='0.5.1'; Commit='9011805fc234938628aebe37b757c2f979acfcf7' }
    )) {
        $source = git show ($entry.Commit + ':packaging/portable/aec-update.ps1')
        if ($LASTEXITCODE) { throw "Cannot read pinned updater $($entry.Version)" }
        $sources[$entry.Version] = $source -join "`n"
    }
} finally { Pop-Location }

$sandbox = Join-Path ([IO.Path]::GetTempPath()) ('aec-release-upgrade-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $sandbox | Out-Null
try {
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $archive = [IO.Compression.ZipFile]::OpenRead($zip)
    try {
        foreach ($entry in $archive.Entries) {
            if ($entry.FullName -notlike "AgentEgressController-$version-portable/*" -or
                $entry.FullName.Contains(':') -or $entry.FullName.Contains('\') -or
                ($entry.FullName -split '/') -contains '..' -or ($entry.FullName -split '/') -contains '.') {
                throw 'Release archive has an unsafe entry'
            }
        }
    } finally { $archive.Dispose() }
    Expand-Archive -LiteralPath $zip -DestinationPath (Join-Path $sandbox 'expanded')
    $package = Join-Path $sandbox "expanded/AgentEgressController-$version-portable"
    if ((Get-FileHash $bootstrap).Hash -ne (Get-FileHash (Join-Path $package 'aec-update.ps1')).Hash) {
        throw 'Bootstrap is not the packaged updater'
    }
    # Lock down the old failure instead of claiming v0.5.0 can directly self-update.
    & {
        . (Get-UpdaterFunctions $sources['0.5.0'])
        $rejected = $false
        try { [void](Get-PackageFiles $package) } catch { $rejected = $true }
        if (-not $rejected -or (Test-PackagePath 'app.ico') -or (Test-PackagePath 'ui/active.json')) {
            throw 'Expected historical whitelist incompatibility was not reproduced'
        }
        Write-Output 'PASS: original 0.5.0 rejects new package before service shutdown; bootstrap required.'
    }
    $cases = @(
        @{ Name='0.5.1-direct'; Source=$sources['0.5.1'] },
        @{ Name='0.5.0-bootstrap'; Source=(Get-Content -LiteralPath $bootstrap -Raw -Encoding UTF8) }
    )
    foreach ($case in $cases) {
        foreach ($portable in @($true, $false)) {
            & {
                . (Get-UpdaterFunctions $case.Source)
                $mode = if ($portable) { 'portable' } else { 'installed' }
                $root = Join-Path $sandbox ($case.Name + '-' + $mode)
                $source = Join-Path $root 'source'
                $target = Join-Path $root 'target'
                $backup = Join-Path $root 'backup'
                New-Item -ItemType Directory -Path $source, $target | Out-Null
                Copy-Item -Path (Join-Path $package '*') -Destination $source -Recurse
                Test-PackageIntegrity $source
                New-Item -ItemType Directory -Path (Join-Path $target 'data/profiles'), (Join-Path $target 'bin') | Out-Null
                $profile = Join-Path $target 'data/profiles/profiles.json'
                $geo = Join-Path $target 'bin/GeoSite.dat'
                [IO.File]::WriteAllText($profile, '{"synthetic":"keep-routes-and-credential-refs"}')
                [IO.File]::WriteAllText($geo, 'synthetic-user-geodata')
                $profileHash = (Get-FileHash $profile).Hash
                $geoHash = (Get-FileHash $geo).Hash
                [IO.File]::WriteAllText((Join-Path $target 'aec-tray.exe'), 'old-tray-fixture')
                [IO.File]::WriteAllText((Join-Path $target 'aec-control.exe'), 'old-control-fixture')
                if ($portable) { [IO.File]::WriteAllText((Join-Path $target 'portable.flag'), '') }
                [void](Prepare-PackageForTarget $source $target)
                $plan = Get-UpdatePlan $source $target $false
                if (-not $plan.CoreStopRequired) { throw 'Binary update did not require core confirmation' }
                $cancelled = Invoke-PlannedUpdate $plan { $false } { throw 'Stopped after cancel' } { throw 'Copied after cancel' } { throw 'Restarted after cancel' }
                if ($cancelled -ne 'cancelled' -or [IO.File]::ReadAllText((Join-Path $target 'aec-control.exe')) -ne 'old-control-fixture') {
                    throw 'Cancellation changed installation'
                }
                $events = [Collections.Generic.List[string]]::new()
                $outcome = Invoke-PlannedUpdate $plan { $events.Add('confirm'); $true } { $events.Add('stop') } {
                    $events.Add('copy'); Invoke-InstallPlan $plan $source $target $backup
                } { param($stopped) if (-not $stopped) { throw 'Stop not completed' }; $events.Add('restart') }
                if ($outcome -ne 'core-updated' -or ($events -join ',') -ne 'confirm,stop,copy,restart') { throw 'Wrong lifecycle ordering' }
                foreach ($file in $plan.Files) {
                    if ((Get-FileHash $file.Source).Hash -ne (Get-FileHash (Join-Path $target $file.Relative)).Hash) {
                        throw "Installed file differs: $($file.Relative)"
                    }
                }
                if ((Test-Path (Join-Path $target 'portable.flag')) -ne $portable) { throw 'Installation mode changed' }
                if ((Get-FileHash $profile).Hash -ne $profileHash -or (Get-FileHash $geo).Hash -ne $geoHash) { throw 'User data changed' }
                if ([IO.File]::ReadAllText((Join-Path $backup 'aec-tray.exe')) -ne 'old-tray-fixture') { throw 'Old program not backed up' }
                if ((Get-UpdatePlan $source $target $false).Files.Count -ne 0) { throw 'Repeat update is not a no-op' }
                Restore-UpdateBackup $target $backup
                if ([IO.File]::ReadAllText((Join-Path $target 'aec-control.exe')) -ne 'old-control-fixture' -or
                    (Test-Path (Join-Path $target 'ui/active.json')) -or (Get-FileHash $profile).Hash -ne $profileHash -or
                    (Get-FileHash $geo).Hash -ne $geoHash) { throw 'Rollback did not restore the old state' }
                Write-Output "PASS: $($case.Name) $mode - real package integrity, core confirmation/cancel, copy, data preservation, backup, no-op, rollback."
            }
        }
    }
    Write-Output 'Release upgrade compatibility passed. No real service or global setting was touched.'
} finally {
    $resolved = [IO.Path]::GetFullPath($sandbox)
    $tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\') + '\'
    if (-not $resolved.StartsWith($tempRoot, [StringComparison]::OrdinalIgnoreCase) -or
        (Split-Path -Leaf $resolved) -notlike 'aec-release-upgrade-*') { throw 'Unsafe test cleanup target' }
    Remove-Item -LiteralPath $resolved -Recurse -Force
}
