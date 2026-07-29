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

Push-Location (Join-Path $root 'src-tauri')
cargo build --release
$built = $LASTEXITCODE
Pop-Location
if ($built -ne 0) { Write-Error "cargo build failed ($built)"; exit 1 }

$dist = Join-Path $root 'dist'
New-Item -ItemType Directory -Force -Path (Join-Path $dist 'models') | Out-Null

Copy-Item (Join-Path $root 'src-tauri\target\release\verba.exe') (Join-Path $dist 'Verba.exe') -Force

# Only copy the model when it is missing or stale - it is 181MB.
Get-ChildItem (Join-Path $root 'models') -Filter '*.bin' | ForEach-Object {
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

