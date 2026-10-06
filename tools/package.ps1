<#
.SYNOPSIS
  Builds the release package: dist\Desktop Dock <version>\ with desktop-dock.exe, README.txt and
  THIRD-PARTY-NOTICES.txt (the licences of the Rust crates built into it), a zip of that folder
  and its SHA-256. The program installs itself: whoever downloads it runs desktop-dock.exe.
  The build has line tables for crash diagnosis in a separate .pdb (dist\symbols, not shipped),
  and no paths from this PC inside the exe (checked).
.EXAMPLE
  .\tools\package.ps1
#>
param([switch]$NoBuild)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$version = ([regex]::Match((Get-Content (Join-Path $root 'Cargo.toml') -Raw), '(?m)^version\s*=\s*"([^"]+)"')).Groups[1].Value
$targetDir = Join-Path $root 'target\package'
$built = Join-Path $targetDir 'release\desktop-dock.exe'

if (-not $NoBuild) {
    # Its own target folder, so normal builds aren't rebuilt; local paths mapped away; the PDB
    # named without its folder; no link time stamped in (/Brepro), so the same source always
    # gives the same exe, byte for byte (so a release can be checked against its source).
    # (Encoded with Cargo's unit separator, so paths with spaces stay whole.)
    $env:CARGO_ENCODED_RUSTFLAGS = @("--remap-path-prefix=$env:USERPROFILE=~", "--remap-path-prefix=$root=.", '-C', 'link-arg=/PDBALTPATH:%_PDB%', '-C', 'link-arg=/Brepro', '-C', 'target-feature=+crt-static') -join [char]0x1f
    $env:CARGO_PROFILE_RELEASE_DEBUG = 'line-tables-only'
    $env:CARGO_PROFILE_RELEASE_STRIP = 'none'
    Push-Location $root
    try { cargo build --release --target-dir $targetDir; if ($LASTEXITCODE) { throw 'The build failed.' } }
    finally { Pop-Location; Remove-Item Env:CARGO_ENCODED_RUSTFLAGS, Env:CARGO_PROFILE_RELEASE_DEBUG, Env:CARGO_PROFILE_RELEASE_STRIP -ErrorAction SilentlyContinue }
}

# Runs on a fresh Windows: no Visual C++ Redistributable needed (its runtime is built in).
if ([Text.Encoding]::ASCII.GetString([IO.File]::ReadAllBytes($built)) -match '(?i)(vcruntime|msvcp)1\d\d(_\d)?\.dll') {
    throw "The exe needs $($Matches[0]), which a fresh Windows doesn't have: build it with the C runtime built in (.cargo\config.toml)."
}

# Nothing from this PC inside the exe: no user folder, no project folder.
$bytes = [IO.File]::ReadAllBytes($built)
$ascii = [Text.Encoding]::ASCII.GetString($bytes)
$wide = [Text.Encoding]::Unicode.GetString($bytes)
foreach ($local in @($env:USERPROFILE, $root, $env:USERNAME) | Where-Object { $_ -and $_.Length -ge 4 }) {
    if ($ascii.IndexOf($local, [StringComparison]::OrdinalIgnoreCase) -ge 0 -or $wide.IndexOf($local, [StringComparison]::OrdinalIgnoreCase) -ge 0) {
        throw "The exe contains '$local': not packaging it."
    }
}
# No behaviour broken on purpose (src\breaks.rs: only with the test-break feature).
foreach ($trace in 'break.txt', 'broken on purpose') {
    if ($ascii.IndexOf($trace, [StringComparison]::Ordinal) -ge 0) { throw "The exe contains '$trace': it was built with the test-break feature. Not packaging it." }
}

$dist = Join-Path $root 'dist'
$folder = Join-Path $dist "Desktop Dock $version"
if (Test-Path $folder) { [IO.Directory]::Delete($folder, $true) }
New-Item -ItemType Directory -Force $folder, (Join-Path $dist 'symbols') | Out-Null
Copy-Item $built (Join-Path $folder 'desktop-dock.exe')
# The README without its pictures (they're on the project page).
Get-Content (Join-Path $root 'README.md') -Encoding UTF8 | Where-Object { $_ -notmatch 'docs/images/|^\s*</?p\b' } | Set-Content (Join-Path $folder 'README.txt') -Encoding UTF8
$pdb = Join-Path $targetDir 'release\desktop_dock.pdb'
if (Test-Path $pdb) { Copy-Item $pdb (Join-Path $dist "symbols\desktop_dock-$version.pdb") -Force }

# The licences of the crates built into the exe (not build tools), each licence text once.
Push-Location $root
try {
    $metadata = cargo metadata --format-version 1 --locked | ConvertFrom-Json
    $shipped = @(cargo tree -e normal --prefix none --format '{p}' --locked | ForEach-Object { ($_ -split ' ')[0..1] -join ' ' } | Where-Object { $_ -notmatch '^desktop-dock ' } | Sort-Object -Unique)
} finally { Pop-Location }
$lines = @("Desktop Dock $version includes these open-source Rust crates. Their licences follow.", '')
$texts = [ordered]@{}
foreach ($entry in $shipped) {
    $name, $ver = $entry -split ' '
    $ver = $ver.TrimStart('v')
    $package = $metadata.packages | Where-Object { $_.name -eq $name -and $_.version -eq $ver } | Select-Object -First 1
    if (-not $package) { continue }
    $lines += "$name $ver  ($($package.license))  $($package.repository)"
    $dir = Split-Path $package.manifest_path
    foreach ($file in Get-ChildItem $dir -File | Where-Object { $_.Name -match '^(LICEN[CS]E|COPYING|NOTICE)' }) {
        $text = (Get-Content $file.FullName -Raw).Trim()
        if (-not $texts.Contains($text)) { $texts[$text] = New-Object System.Collections.Generic.List[string] }
        $texts[$text].Add("$name $ver")
    }
}
foreach ($text in $texts.Keys) {
    $lines += '', ('=' * 78), ("Used by: " + ($texts[$text] -join ', ')), ('=' * 78), '', $text
}
[IO.File]::WriteAllLines((Join-Path $folder 'THIRD-PARTY-NOTICES.txt'), $lines, (New-Object Text.UTF8Encoding $false))

$zip = Join-Path $dist "DesktopDock-$version.zip"
if (Test-Path $zip) { [IO.File]::Delete($zip) }
Compress-Archive -Path (Join-Path $folder '*') -DestinationPath $zip
$hash = (Get-FileHash $zip -Algorithm SHA256).Hash
"$hash  DesktopDock-$version.zip" | Set-Content (Join-Path $dist "DesktopDock-$version.zip.sha256") -Encoding ASCII
"Packaged Desktop Dock $version"
"  $folder (exe $([math]::Round((Get-Item (Join-Path $folder 'desktop-dock.exe')).Length / 1KB)) KB, README.txt, THIRD-PARTY-NOTICES.txt: $($shipped.Count) crates)"
"  $zip"
"  SHA-256 $hash"
# (The exe's own: what a build from the same source gives, byte for byte.)
"  exe SHA-256 $((Get-FileHash $built -Algorithm SHA256).Hash)"
"  symbols: dist\symbols\desktop_dock-$version.pdb (keep; not shipped)"
