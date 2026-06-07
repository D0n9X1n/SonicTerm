# Windows packaging

The MSI is built by `cargo wix` from `sonicterm-windows/wix/main.wxs`.
On Windows, install once:

```powershell
cargo install cargo-wix --locked
choco install wixtoolset --no-progress -y
```

Then from the repo root:

```powershell
. .\scripts\setup-windows-cairo.ps1
cargo build --release --target x86_64-pc-windows-msvc -p sonicterm-windows
cargo wix --package sonicterm-windows --no-build --output dist\
```

The release workflow does this automatically when a `v*` tag is pushed.
