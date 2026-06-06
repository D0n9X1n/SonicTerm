# Windows packaging

The MSI is built by `cargo wix` from `sonicterm-windows/wix/main.wxs`.
On Windows, install once:

```powershell
cargo install cargo-wix --locked
```

Then from the repo root:

```powershell
cd crates/sonicterm-windows
cargo wix --no-build --output ../../dist/
```

The release workflow does this automatically when a `v*` tag is pushed.
