# Windows packaging

Packaging: [macOS](macos.md) · **Windows** · [Index](README.md)

The MSI is built by `cargo wix` from
`crates/sonicterm-windows/wix/main.wxs`. Packaging automation is kept with the
other first-party entry points in `scripts/`.

On Windows, install Rust's MSVC target, vcpkg, cargo-wix, and WiX Toolset 3.
Make `vcpkg.exe` available through `VCPKG_ROOT`,
`VCPKG_INSTALLATION_ROOT`, or `C:\vcpkg` before running the setup script:

```powershell
rustup target add x86_64-pc-windows-msvc
cargo install cargo-wix --locked
choco install wixtoolset --no-progress -y
```

Restart the shell after installing WiX if its `bin` directory is not yet on
`PATH`. Then run the same package sequence as the release workflow from the
repository root:

```powershell
. .\scripts\setup-windows-cairo.ps1
cargo build --release --target x86_64-pc-windows-msvc -p sonicterm-windows
New-Item -ItemType Directory -Force -Path dist | Out-Null
Push-Location .\crates\sonicterm-windows
cargo wix --package sonicterm-windows --no-build --nocapture --output ..\..\dist\
Pop-Location
```

The release workflow performs these steps automatically when a `v*` tag is
pushed. Local packaging only writes the MSI under `dist/`.
