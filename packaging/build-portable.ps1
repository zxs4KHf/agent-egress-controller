param([string]$MihomoPath = "runtime/bin/aec-mihomo.exe")
$ErrorActionPreference = "Stop"

function New-PortableArchive([string]$SourceRoot, [string]$Destination, [string]$ArchiveRoot) {
    Add-Type -AssemblyName System.IO.Compression
    $minimumTime = [datetime]::SpecifyKind([datetime]'1980-01-01T00:00:00', [DateTimeKind]::Local)
    $maximumTime = [datetime]::SpecifyKind([datetime]'2107-12-31T23:59:58', [DateTimeKind]::Local)
    $archiveFile = [IO.File]::Open($Destination, [IO.FileMode]::CreateNew, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
    $archive = $null
    try {
        $archive = [IO.Compression.ZipArchive]::new($archiveFile, [IO.Compression.ZipArchiveMode]::Create, $false)
        $rootPath = [IO.Path]::GetFullPath($SourceRoot).TrimEnd('\') + '\'
        Get-ChildItem -LiteralPath $SourceRoot -Recurse -File | ForEach-Object {
            $relative = $_.FullName.Substring($rootPath.Length).Replace('\', '/')
            $entry = $archive.CreateEntry("$ArchiveRoot/$relative", [IO.Compression.CompressionLevel]::Optimal)
            $timestamp = $_.LastWriteTime
            if ($timestamp -lt $minimumTime) { $timestamp = $minimumTime }
            if ($timestamp -gt $maximumTime) { $timestamp = $maximumTime }
            $entry.LastWriteTime = [DateTimeOffset]$timestamp
            $input = [IO.File]::OpenRead($_.FullName)
            try {
                $outputStream = $entry.Open()
                try { $input.CopyTo($outputStream) } finally { $outputStream.Dispose() }
            } finally { $input.Dispose() }
        }
    } finally {
        if ($archive) { $archive.Dispose() }
        $archiveFile.Dispose()
    }
}

$repo = Split-Path -Parent $PSScriptRoot
Push-Location $repo
try {
    $manifest = Get-Content -LiteralPath (Join-Path $repo "Cargo.toml") -Raw
    $version = [regex]::Match($manifest, 'version = "([0-9.]+)"').Groups[1].Value
    if (-not $version) { throw "Cannot determine workspace version" }
    $core = (Resolve-Path -LiteralPath $MihomoPath).Path
    $pin = Get-Content -LiteralPath (Join-Path $repo "crates/aec-core/src/core/pin.rs") -Raw
    $expectedHash = [regex]::Match($pin, 'EXE_SHA256: &str = "([0-9a-f]{64})"').Groups[1].Value
    if ((Get-FileHash -LiteralPath $core -Algorithm SHA256).Hash.ToLowerInvariant() -ne $expectedHash) {
        throw "mihomo checksum does not match the pinned binary"
    }
    cargo fmt --all --check
    if ($LASTEXITCODE) { throw "Format check failed" }
    cargo clippy --all-targets -- -D warnings
    if ($LASTEXITCODE) { throw "Clippy failed" }
    cargo test
    if ($LASTEXITCODE) { throw "Tests failed" }
    cargo build --release --workspace --locked
    if ($LASTEXITCODE) { throw "Release build failed" }
    $name = "AgentEgressController-$version-portable"
    $dist = Join-Path $repo "dist"
    $output = Join-Path $dist $name
    $zip = Join-Path $dist "$name.zip"
    if ((Test-Path -LiteralPath $output) -or (Test-Path -LiteralPath $zip)) {
        throw "Release output already exists; preserve it and use a fresh version or move it aside"
    }
    New-Item -ItemType Directory -Path (Join-Path $output "bin"), (Join-Path $output "docs") -Force | Out-Null
    foreach ($exe in @("aec-tray.exe", "aec-dev.exe", "aec-relay.exe", "aec-agent.exe", "aec-control.exe")) {
        Copy-Item -LiteralPath (Join-Path $repo "target/release/$exe") -Destination (Join-Path $output $exe)
    }
    Copy-Item -LiteralPath $core -Destination (Join-Path $output "bin/aec-mihomo.exe")
    Copy-Item -LiteralPath (Join-Path $repo "packaging/portable/aec-update.ps1") -Destination (Join-Path $output "aec-update.ps1")
    foreach ($file in @("README.md", "LICENSE", "THIRD-PARTY-NOTICES.md", "CHANGELOG.md", "CONTEXT.md", "CONTRIBUTING.md", "AGENTS.md", "SECURITY.md")) {
        Copy-Item -LiteralPath (Join-Path $repo $file) -Destination (Join-Path $output $file)
    }
    # Copy only Git-visible public docs/assets; ignored maintainer runbooks never enter the zip.
    $publicFiles = git ls-files -- docs assets/readme
    if ($LASTEXITCODE) { throw "Cannot enumerate public documentation" }
    foreach ($file in $publicFiles) {
        $destination = Join-Path $output $file
        New-Item -ItemType Directory -Path (Split-Path -Parent $destination) -Force | Out-Null
        Copy-Item -LiteralPath (Join-Path $repo $file) -Destination $destination
    }
    foreach ($file in @("USER-GUIDE.md", "AGENT-INTEGRATION.md", "RELEASE-0.5.0.md", "RELEASE-0.4.0.md", "RELEASE-0.2.0.md")) {
        Copy-Item -LiteralPath (Join-Path $repo "docs/$file") -Destination (Join-Path $output "docs/$file")
    }
    # Keep the script ASCII so Windows PowerShell 5.1 also reads it correctly without a BOM.
    $instructionName = (-join [char[]]@(0x4F7F, 0x7528, 0x8BF4, 0x660E)) + ".txt"
    Copy-Item -LiteralPath (Join-Path $repo "packaging/portable/$instructionName") -Destination (Join-Path $output $instructionName)
    Copy-Item -LiteralPath (Join-Path $repo "crates/aec-core/tests/fixtures/routing.yaml") -Destination (Join-Path $output "docs/routing-example.yaml")
    # Only a marker, never runtime data. This forces the packaged tools onto ./data.
    New-Item -ItemType File -Path (Join-Path $output "portable.flag") | Out-Null
    # Retain the license/copyright texts of every locked dependency present in the local cache.
    $cargoCache = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE ".cargo" }
    $registries = Get-ChildItem -LiteralPath (Join-Path $cargoCache "registry/src") -Directory
    $lock = Get-Content -LiteralPath (Join-Path $repo "Cargo.lock") -Raw
    $packages = [regex]::Matches($lock, '(?ms)\[\[package\]\]\s+name = "([^"]+)"\s+version = "([^"]+)"\s+source = "registry')
    $metadataOutput = & cargo metadata --format-version 1 --filter-platform x86_64-pc-windows-msvc --locked
    if ($LASTEXITCODE) { throw "Cannot resolve Windows package dependencies" }
    $metadata = ($metadataOutput -join [Environment]::NewLine) | ConvertFrom-Json
    $usedRegistryPackages = @{}
    foreach ($node in $metadata.resolve.nodes) {
        if ($node.id -match '#([^@]+)@(.+)$') {
            $usedRegistryPackages["$($Matches[1])-$($Matches[2])"] = $true
        }
    }
    foreach ($package in $packages) {
        $folder = "$($package.Groups[1].Value)-$($package.Groups[2].Value)"
        if (-not $usedRegistryPackages.ContainsKey($folder)) { continue }
        $source = $registries | ForEach-Object { Join-Path $_.FullName $folder } | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
        if (-not $source) { continue } # unused cross-target crates are not in this binary
        $licenses = Get-ChildItem -LiteralPath $source -File | Where-Object { $_.Name -match '^(LICENSE|LICENCE|COPYING|NOTICE)' }
        $destination = Join-Path $output "licenses/$folder"
        New-Item -ItemType Directory -Path $destination -Force | Out-Null
        if ($licenses) {
            $licenses | ForEach-Object { Copy-Item -LiteralPath $_.FullName -Destination (Join-Path $destination $_.Name) }
        } elseif ($folder -like 'webview2-com-*') {
            Copy-Item -LiteralPath (Join-Path $repo 'packaging/licenses/webview2-rs-MIT.txt') -Destination (Join-Path $destination 'LICENSE-MIT.txt')
        } else {
            throw "Dependency license text missing: $folder"
        }
    }
    $sumLines = Get-ChildItem -LiteralPath $output -Recurse -File | Sort-Object FullName | ForEach-Object {
        $relative = $_.FullName.Substring($output.Length + 1).Replace('\', '/')
        "$((Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant())  $relative"
    }
    $sumsPath = Join-Path $output "SHA256SUMS.txt"
    $sumLines | Set-Content -LiteralPath $sumsPath -Encoding utf8
    New-PortableArchive -SourceRoot $output -Destination $zip -ArchiveRoot $name
    $zipHash = (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash.ToLowerInvariant()
    $zipSums = Join-Path $dist "SHA256SUMS-$version.txt"
    "$zipHash  $name.zip" | Set-Content -LiteralPath $zipSums -Encoding utf8
    $makensis = Get-Command makensis.exe -ErrorAction SilentlyContinue
    if (-not $makensis) {
        $defaultMakensis = Join-Path ([Environment]::GetFolderPath('ProgramFilesX86')) 'NSIS\makensis.exe'
        if (Test-Path -LiteralPath $defaultMakensis -PathType Leaf) {
            $makensis = [pscustomobject]@{ Source = $defaultMakensis }
        }
    }
    if ($makensis) {
        $installer = Join-Path $dist "AgentEgressController-$version-setup.exe"
        & $makensis.Source "/DPackageDir=$output" "/DProductVersion=$version" "/DInstallerOut=$installer" (Join-Path $repo "packaging/installer.nsi")
        if ($LASTEXITCODE) { throw "NSIS installer build failed" }
        $installerHash = (Get-FileHash -LiteralPath $installer -Algorithm SHA256).Hash.ToLowerInvariant()
        "$installerHash  $(Split-Path -Leaf $installer)" | Set-Content -LiteralPath (Join-Path $dist "SHA256SETUP-$version.txt") -Encoding utf8
        Write-Output "Built $installer"
        Write-Output "SHA256 $installerHash"
    } else {
        Write-Warning "NSIS is not installed; portable zip built without the optional setup.exe"
    }
    Write-Output "Built $zip"
    Write-Output "SHA256 $zipHash"
} finally { Pop-Location }
