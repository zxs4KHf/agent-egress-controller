param([string]$MihomoPath = "runtime/bin/aec-mihomo.exe")
$ErrorActionPreference = "Stop"
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
    foreach ($exe in @("aec-tray.exe", "aec-dev.exe", "aec-relay.exe")) {
        Copy-Item -LiteralPath (Join-Path $repo "target/release/$exe") -Destination (Join-Path $output $exe)
    }
    Copy-Item -LiteralPath $core -Destination (Join-Path $output "bin/aec-mihomo.exe")
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
    foreach ($file in @("USER-GUIDE.md", "RELEASE-0.2.0.md")) {
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
    foreach ($package in $packages) {
        $folder = "$($package.Groups[1].Value)-$($package.Groups[2].Value)"
        $source = $registries | ForEach-Object { Join-Path $_.FullName $folder } | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
        if (-not $source) { continue } # unused cross-target crates are not in this binary
        $licenses = Get-ChildItem -LiteralPath $source -File | Where-Object { $_.Name -match '^(LICENSE|LICENCE|COPYING|NOTICE)' }
        if (-not $licenses) { throw "Dependency license text missing: $folder" }
        $destination = Join-Path $output "licenses/$folder"
        New-Item -ItemType Directory -Path $destination -Force | Out-Null
        $licenses | ForEach-Object { Copy-Item -LiteralPath $_.FullName -Destination (Join-Path $destination $_.Name) }
    }
    $sumLines = Get-ChildItem -LiteralPath $output -Recurse -File | Sort-Object FullName | ForEach-Object {
        $relative = $_.FullName.Substring($output.Length + 1).Replace('\', '/')
        "$((Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant())  $relative"
    }
    $sumsPath = Join-Path $output "SHA256SUMS.txt"
    $sumLines | Set-Content -LiteralPath $sumsPath -Encoding utf8
    Compress-Archive -LiteralPath $output -DestinationPath $zip -CompressionLevel Optimal
    $zipHash = (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash.ToLowerInvariant()
    $zipSums = Join-Path $dist "SHA256SUMS-$version.txt"
    "$zipHash  $name.zip" | Set-Content -LiteralPath $zipSums -Encoding utf8
    Write-Output "Built $zip"
    Write-Output "SHA256 $zipHash"
} finally { Pop-Location }
