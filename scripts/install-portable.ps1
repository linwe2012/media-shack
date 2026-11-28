param(
    [string]$TargetDir = "E:\Programs\media-shack",
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"
$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$Manifest = Join-Path $RepoRoot "src-tauri\Cargo.toml"

if (-not $SkipBuild) {
    Write-Host "Building release binaries (media-shack, video-recover)..."
    cargo build --release --manifest-path $Manifest --bin media-shack --bin video-recover
    if ($LASTEXITCODE -ne 0) {
        throw "Cargo release build failed with exit code $LASTEXITCODE"
    }
}

$ReleaseDir = Join-Path $RepoRoot "src-tauri\target\release"
$MainExe = Join-Path $ReleaseDir "media-shack.exe"
$RecoverExe = Join-Path $ReleaseDir "video-recover.exe"
if (-not (Test-Path $MainExe)) { throw "Missing release binary: $MainExe" }

# Stop running Media Shack instance if updating an existing install
Get-Process -Name "media-shack" -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 300

# Create portable target directories WITHOUT removing or overwriting existing data/cache
New-Item -ItemType Directory -Force -Path $TargetDir | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $TargetDir "binaries") | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $TargetDir "workers") | Out-Null
$DataDir = Join-Path $TargetDir "data"
New-Item -ItemType Directory -Force -Path $DataDir | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $DataDir "analysis_cache") | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $DataDir "previews") | Out-Null

# Replace executables and sidecars only (never touches data\media_shack.db, data\analysis_cache, or data\previews)
Copy-Item $MainExe (Join-Path $TargetDir "media-shack.exe") -Force
if (Test-Path $RecoverExe) {
    Copy-Item $RecoverExe (Join-Path $TargetDir "video-recover.exe") -Force
}

$WorkerSrc = Join-Path $RepoRoot "workers\ml_worker.py"
if (Test-Path $WorkerSrc) {
    Copy-Item $WorkerSrc (Join-Path $TargetDir "workers\ml_worker.py") -Force
}

# Copy FFmpeg / FFprobe sidecars if available from repo binaries, E:\bin, or PATH
function Find-ToolExe([string]$name, [string[]]$extraCandidates) {
    foreach ($cand in $extraCandidates) {
        if ($cand -and (Test-Path $cand)) { return $cand }
    }
    $cmd = Get-Command $name -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($cmd -and $cmd.Source -and (Test-Path $cmd.Source)) { return $cmd.Source }
    return $null
}

$ffmpegSrc = Find-ToolExe "ffmpeg.exe" @(
    (Join-Path $RepoRoot "src-tauri\binaries\ffmpeg-x86_64-pc-windows-msvc.exe"),
    (Join-Path $RepoRoot "src-tauri\binaries\ffmpeg.exe"),
    "E:\bin\ffmpeg.exe"
)
$ffprobeSrc = Find-ToolExe "ffprobe.exe" @(
    (Join-Path $RepoRoot "src-tauri\binaries\ffprobe-x86_64-pc-windows-msvc.exe"),
    (Join-Path $RepoRoot "src-tauri\binaries\ffprobe.exe"),
    "E:\bin\ffprobe.exe"
)
if ($ffmpegSrc) {
    Copy-Item $ffmpegSrc (Join-Path $TargetDir "binaries\ffmpeg.exe") -Force
}
if ($ffprobeSrc) {
    Copy-Item $ffprobeSrc (Join-Path $TargetDir "binaries\ffprobe.exe") -Force
}

# Marker enabling self-contained <exe_dir>\data storage
Set-Content -Path (Join-Path $TargetDir "portable.flag") -Value "portable=true`ndata_dir=.\data`n" -Encoding UTF8
Set-Content -Path (Join-Path $TargetDir "PORTABLE_README.txt") -Value @"
Media Shack (Portable Edition)
==============================

- Executable: media-shack.exe
- CLI Recovery Tool: video-recover.exe
- Bundled Sidecars: binaries\ffmpeg.exe, binaries\ffprobe.exe, workers\ml_worker.py
- Persistent Data & Cache: .\data\ (media_shack.db, analysis_cache\, previews\)

Reinstalling / Upgrading:
  Re-run scripts\install-portable.ps1 -TargetDir "$TargetDir" (or replace media-shack.exe,
  video-recover.exe, binaries\, and workers\ directly). The .\data\ directory containing your
  SQLite catalog, thumbnails, previews, and analysis cache is always preserved.
"@ -Encoding UTF8

Write-Host "Portable installation complete at: $TargetDir"
Write-Host "Persistent data & cache directory (preserved on reinstall): $DataDir"
