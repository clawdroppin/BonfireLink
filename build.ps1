# Builds the Windows and Linux (Steam Deck) apps and packages them into dist\.
# Linux cross-compiling needs (one-time): rustup target add x86_64-unknown-linux-gnu,
# cargo install cargo-zigbuild, and zig (python -m pip install --user ziglang).
# Failures are detected through exit codes: cargo prints progress on stderr,
# which Windows PowerShell would otherwise treat as an error.
$ErrorActionPreference = "Continue"
Set-Location $PSScriptRoot

cargo test --quiet
if ($LASTEXITCODE -ne 0) { throw "tests failed" }

cargo build --release
if ($LASTEXITCODE -ne 0) { throw "Windows build failed" }

if (-not $env:CARGO_ZIGBUILD_ZIG_PATH) {
    $zig = python -c "import ziglang, os; print(os.path.join(os.path.dirname(ziglang.__file__), 'zig.exe'))" 2>$null
    if ($zig -and (Test-Path $zig)) { $env:CARGO_ZIGBUILD_ZIG_PATH = $zig }
}
# glibc 2.28 keeps the binary compatible with every SteamOS 3 release.
cargo zigbuild --release --target x86_64-unknown-linux-gnu.2.28
if ($LASTEXITCODE -ne 0) { throw "Linux build failed" }

python -I scripts\check_elf.py target\x86_64-unknown-linux-gnu\release\BonfireLink
if ($LASTEXITCODE -ne 0) { throw "Linux binary links something SteamOS may not have" }

python -I scripts\package.py
if ($LASTEXITCODE -ne 0) { throw "packaging failed" }
