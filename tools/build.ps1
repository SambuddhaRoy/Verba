# Builds a standalone, double-clickable Verba.
#
#   powershell -NoProfile -File tools/build.ps1
#
# Produces dist/Verba.exe alongside dist/models/. The exe resolves the model
# relative to itself, so the folder can be moved anywhere.

# Deliberately not ErrorActionPreference='Stop': cargo writes its progress to
# stderr, and under 'Stop' PowerShell treats each of those lines as a
# terminating error. $LASTEXITCODE is the only reliable signal here.
$root = Split-Path $PSScriptRoot -Parent

# whisper.cpp's Vulkan backend builds vulkan-shaders-gen as a nested
# ExternalProject. Under the default target dir the deepest MSBuild tracking
# log lands at ~265 characters, five past MAX_PATH, and the build fails with
# FileTracker FTK1011. A short target dir buys back 32 characters.
if (-not $env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR = 'C:\vb' }
New-Item -ItemType Directory -Force -Path $env:CARGO_TARGET_DIR | Out-Null

if (-not $env:VULKAN_SDK) {
  $sdk = Get-ChildItem 'C:\VulkanSDK' -Directory -EA SilentlyContinue |
         Sort-Object Name -Descending | Select-Object -First 1
  if ($sdk) {
    $env:VULKAN_SDK = $sdk.FullName
  } else {
    Write-Warning "No Vulkan SDK found. Build with --no-default-features for a CPU-only binary."
  }
}
Write-Host "VULKAN_SDK    = $env:VULKAN_SDK"
Write-Host "target dir    = $env:CARGO_TARGET_DIR"

Push-Location (Join-Path $root 'src-tauri')
cargo build --release
$built = $LASTEXITCODE
Pop-Location
if ($built -ne 0) { Write-Error "cargo build failed ($built)"; exit 1 }

$dist = Join-Path $root 'dist'
New-Item -ItemType Directory -Force -Path (Join-Path $dist 'models') | Out-Null

Copy-Item (Join-Path $env:CARGO_TARGET_DIR 'release\verba.exe') (Join-Path $dist 'Verba.exe') -Force

# Only copy models that are missing or stale - they are large.
Get-ChildItem (Join-Path $root 'models') -Filter '*.bin' -EA SilentlyContinue | ForEach-Object {
  $target = Join-Path $dist "models\$($_.Name)"
  if (-not (Test-Path $target) -or (Get-Item $target).Length -ne $_.Length) {
    Write-Host "copying $($_.Name)..."
    Copy-Item $_.FullName $target -Force
  }
}

Write-Host ""
Write-Host "dist\Verba.exe ready:"
Get-ChildItem $dist -Recurse -File |
  Select-Object @{n='file';e={$_.FullName.Substring($dist.Length+1)}},
                @{n='MB';e={[math]::Round($_.Length/1MB,1)}}
