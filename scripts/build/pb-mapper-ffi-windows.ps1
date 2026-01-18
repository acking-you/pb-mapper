# Build Windows FFI DLL and stage it for Flutter.
# - Copies to ui/native/windows/x64 for packaging
# - Copies into existing build output folders for local runs

$ErrorActionPreference = "Stop"

$root = Resolve-Path (Join-Path $PSScriptRoot "../..")
$ffiLib = "pb_mapper_ffi.dll"

Push-Location $root
try {
  cargo build -p pb-mapper-ffi --release

  $nativeDir = Join-Path $root "ui/native/windows/x64"
  New-Item -ItemType Directory -Force $nativeDir | Out-Null
  Copy-Item (Join-Path $root "target/release/$ffiLib") (Join-Path $nativeDir $ffiLib) -Force

  $candidateDirs = @(
    Join-Path $root "ui/build/windows/x64/runner/Release",
    Join-Path $root "ui/build/windows/x64/runner/Debug",
    Join-Path $root "ui/build/windows/x64/runner/Profile"
  )

  foreach ($dir in $candidateDirs) {
    if (Test-Path $dir) {
      Copy-Item (Join-Path $root "target/release/$ffiLib") (Join-Path $dir $ffiLib) -Force
    }
  }

  Write-Host "FFI ready: $nativeDir\$ffiLib"
} finally {
  Pop-Location
}
