param([Parameter(Mandatory=$true)][string]$Package, [Parameter(Mandatory=$true)][string]$Version, [Parameter(Mandatory=$true)][string]$Output)
$ErrorActionPreference='Stop'
Add-Type -AssemblyName System.IO.Compression.FileSystem
Add-Type -AssemblyName System.IO.Compression
# Functions only: never run the interactive updater during packaging.
$tokens=$null; $errors=$null
$ast=[Management.Automation.Language.Parser]::ParseFile((Join-Path $PSScriptRoot 'portable/aec-update.ps1'),[ref]$tokens,[ref]$errors)
if ($errors.Count) { throw 'Updater does not parse' }
. ([scriptblock]::Create((@($ast.EndBlock.Statements | Where-Object { $_ -is [Management.Automation.Language.FunctionDefinitionAst] } | ForEach-Object { $_.Extent.Text }) -join "`n")))
if ($Version -notmatch '^\d+\.\d+\.\d+$') { throw 'Invalid release version' }
Test-PackageIntegrity $Package
New-Item -ItemType Directory -Path $Output -Force | Out-Null
$files=@(Get-PackageFiles $Package | ForEach-Object {
    $id=switch -Regex ($_.Relative) {
        '^bin/aec-mihomo.exe$' { 'core'; break }
        '^aec-.*\.exe$' { [IO.Path]::GetFileNameWithoutExtension($_); break }
        '^aec-update.ps1$' { 'updater'; break }
        '^ui/' { 'ui'; break }
        default { 'resources' }
    }
    [pscustomobject]@{ path=$_.Relative; component=$id; size=(Get-Item -LiteralPath $_.Source).Length; sha256=(Get-FileHash -LiteralPath $_.Source).Hash.ToLowerInvariant() }
})
$components=@()
foreach ($group in ($files | Group-Object component)) {
    $asset="aec-$Version-$($group.Name).zip"
    $path=Join-Path $Output $asset
    $stream=[IO.File]::Open($path,[IO.FileMode]::CreateNew)
    $zip=$null
    try {
        $zip=[IO.Compression.ZipArchive]::new($stream,[IO.Compression.ZipArchiveMode]::Create,$false)
        foreach ($file in $group.Group) {
            $entry=$zip.CreateEntry($file.path,[IO.Compression.CompressionLevel]::Optimal)
            $entry.LastWriteTime=[DateTimeOffset]::new(2000,1,1,0,0,0,[TimeSpan]::Zero)
            $input=[IO.File]::OpenRead((Join-Path $Package $file.path))
            try { $outputStream=$entry.Open(); try { $input.CopyTo($outputStream) } finally { $outputStream.Dispose() } } finally { $input.Dispose() }
        }
    } finally { if ($zip) { $zip.Dispose() }; $stream.Dispose() }
    $components += [pscustomobject]@{ id=$group.Name; asset=$asset; size=(Get-Item -LiteralPath $path).Length; sha256=(Get-FileHash -LiteralPath $path).Hash.ToLowerInvariant() }
}
$manifest=[ordered]@{ schemaVersion=1; version=$Version; components=$components; files=$files }
Assert-IncrementalManifest ([pscustomobject]$manifest) $Version
$name="aec-incremental-$Version.json"
$destination=Join-Path $Output $name
if (Test-Path -LiteralPath $destination) { throw 'Incremental manifest already exists' }
[IO.File]::WriteAllText($destination,($manifest | ConvertTo-Json -Depth 8),[Text.UTF8Encoding]::new($false))
((Get-FileHash -LiteralPath $destination).Hash.ToLowerInvariant() + '  ' + $name) | Set-Content -LiteralPath (Join-Path $Output "SHA256INCREMENTAL-$Version.txt") -Encoding UTF8
Write-Output "Built incremental feed: $($components.Count) components, $($files.Count) files."
