# Build Windows FFI DLL and stage it for Flutter.
# - Copies to ui/native/windows/x64 for packaging
# - Copies into existing build output folders for local runs

$ErrorActionPreference = "Stop"

$root = (Resolve-Path (Join-Path $PSScriptRoot "../..") | Select-Object -First 1 -ExpandProperty Path)
$ffiLib = "pb_mapper_ffi.dll"

Push-Location $root
try {
  cargo build -p pb-mapper-ffi --release
  if ($LASTEXITCODE -ne 0) { throw "FFI build failed" }
  cargo build -p pb-mapper-desktop --features windows-helper --bin pb-mapper-desktop-helper --release
  if ($LASTEXITCODE -ne 0) { throw "Desktop helper build failed" }

  $nativeDir = Join-Path $root "ui/native/windows/x64"
  New-Item -ItemType Directory -Force $nativeDir | Out-Null
  Copy-Item (Join-Path $root "target/release/$ffiLib") (Join-Path $nativeDir $ffiLib) -Force
  Copy-Item (Join-Path $root "target/release/pb-mapper-desktop-helper.exe") (Join-Path $nativeDir "pb-mapper-desktop-helper.exe") -Force

  $candidateDirs = @(
    (Join-Path -Path $root -ChildPath "ui/build/windows/x64/runner/Release");
    (Join-Path -Path $root -ChildPath "ui/build/windows/x64/runner/Debug");
    (Join-Path -Path $root -ChildPath "ui/build/windows/x64/runner/Profile")
  )

  foreach ($dir in $candidateDirs) {
    if (Test-Path $dir) {
      Copy-Item (Join-Path $root "target/release/$ffiLib") (Join-Path $dir $ffiLib) -Force
      Copy-Item (Join-Path $root "target/release/pb-mapper-desktop-helper.exe") (Join-Path $dir "pb-mapper-desktop-helper.exe") -Force
    }
  }

  Write-Host "FFI ready: $nativeDir\$ffiLib"
} finally {
  Pop-Location
}
