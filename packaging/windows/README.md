# Windows packaging

The MSI is built by `cargo wix` from `sonic-windows/wix/main.wxs`.
On Windows, install once:

```powershell
cargo install cargo-wix --locked
```

Then from the repo root (the `sonic-windows` crate lives at the top level —
the legacy `crates/` directory has been removed):

```powershell
cd sonic-windows
cargo wix --output ../dist/
```

The release workflow does this automatically when a `v*` tag is pushed.
