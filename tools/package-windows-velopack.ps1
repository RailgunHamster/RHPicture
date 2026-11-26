<#
.SYNOPSIS
    Build RHPicture release binary and pack Velopack installer, portable zip, and incremental delta packages.

.DESCRIPTION
    To generate incremental delta packages (*-delta.nupkg), keep previous versions' *-full.nupkg
    inside the same -OutputDirectory (default: dist\releases) or pass -PreviousReleaseDir.
    Velopack's vpk.exe automatically diffs against existing full packages and updates releases.win.json.
#>
param(
    [string]$Version = '',
    [string]$OutputDirectory = '',
    [string]$PreviousReleaseDir = '',
    [string]$ReleaseNotes = '',
    [string]$VpkPath = '',
    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent

# Read version from Cargo.toml if not specified
$cargoToml = Get-Content -LiteralPath (Join-Path $repo 'Cargo.toml') -Raw
if (-not $Version) {
    if ($cargoToml -match '(?m)^version\s*=\s*"(\d+\.\d+\.\d+(?:-[A-Za-z0-9.]+)?)"') {
        $Version = $Matches[1]
    } else {
        throw 'Cannot determine version from Cargo.toml'
    }
} elseif ($cargoToml -notmatch ('(?m)^version\s*=\s*"' + [regex]::Escape($Version) + '"')) {
    throw "Cargo.toml version does not match requested -Version $Version"
}

# Locate vpk.exe
if (-not $VpkPath) {
    $candidates = @(
        'D:\RHRemoteTools\Velopack\vpk.exe',
        (Get-Command vpk -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Source)
    ) | Where-Object { $_ -and (Test-Path -LiteralPath $_) }
    $VpkPath = $candidates | Select-Object -First 1
}
if (-not $VpkPath -or -not (Test-Path -LiteralPath $VpkPath)) {
    throw 'vpk.exe not found. Install Velopack CLI (dotnet tool install -g vpk) or pass -VpkPath.'
}

$out = if ($OutputDirectory) {
    [IO.Path]::GetFullPath($OutputDirectory)
} else {
    Join-Path $repo 'dist\releases'
}
New-Item -ItemType Directory -Force -Path $out | Out-Null

# Copy previous full packages if provided separately so vpk generates incremental delta nupkg
if ($PreviousReleaseDir -and (Test-Path -LiteralPath $PreviousReleaseDir)) {
    Get-ChildItem -LiteralPath $PreviousReleaseDir -Filter 'RHPicture-*-full.nupkg' -File | ForEach-Object {
        $dest = Join-Path $out $_.Name
        if (-not (Test-Path -LiteralPath $dest)) {
            Copy-Item -LiteralPath $_.FullName -Destination $dest
        }
    }
    $prevFeed = Join-Path $PreviousReleaseDir 'releases.win.json'
    if ((Test-Path -LiteralPath $prevFeed) -and -not (Test-Path -LiteralPath (Join-Path $out 'releases.win.json'))) {
        Copy-Item -LiteralPath $prevFeed -Destination (Join-Path $out 'releases.win.json')
    }
}

$stage = Join-Path $repo ('dist\stage-' + [guid]::NewGuid().ToString('N'))
Push-Location $repo
try {
    if (-not $SkipBuild) {
        & cargo build --release
        if ($LASTEXITCODE -ne 0) { throw 'cargo build --release failed' }
    }

    $exePath = Join-Path $repo 'target\release\RHPicture.exe'
    if (-not (Test-Path -LiteralPath $exePath)) {
        throw "Release executable not found: $exePath"
    }

    New-Item -ItemType Directory -Force -Path $stage | Out-Null
    Copy-Item -LiteralPath $exePath -Destination (Join-Path $stage 'RHPicture.exe')

    $iconPath = Join-Path $repo 'assets\rhpicture.ico'
    $vpkArgs = @(
        'pack',
        '--packId', 'RHPicture',
        '--packVersion', $Version,
        '--packDir', $stage,
        '--mainExe', 'RHPicture.exe',
        '--packAuthors', 'RailgunHamster',
        '--packTitle', 'RHPicture',
        '--channel', 'win',
        '--runtime', 'win-x64',
        '--outputDir', $out
    )
    if (Test-Path -LiteralPath $iconPath) {
        $vpkArgs += @('--icon', $iconPath)
    }
    if ($ReleaseNotes -and (Test-Path -LiteralPath $ReleaseNotes)) {
        $vpkArgs += @('--releaseNotes', (Resolve-Path -LiteralPath $ReleaseNotes).Path)
    }

    Write-Host "Running: $VpkPath $($vpkArgs -join ' ')"
    & $VpkPath @vpkArgs
    if ($LASTEXITCODE -ne 0) { throw 'Velopack packaging failed' }

    Write-Host "`nPackaged artifacts in ${out}:"
    Get-ChildItem -LiteralPath $out -File | Sort-Object Name | ForEach-Object {
        [pscustomobject]@{
            Name   = $_.Name
            SizeKB = [math]::Round($_.Length / 1KB, 1)
            SHA256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
        }
    } | Format-Table -AutoSize
} finally {
    if (Test-Path -LiteralPath $stage) {
        Remove-Item -LiteralPath $stage -Recurse -Force -ErrorAction SilentlyContinue
    }
    Pop-Location
}
