# sonicterm-cfg

## Purpose
Config / theme / keymap / URL safety. Pure-data — loads TOML, validates
keymap actions, validates URLs before spawn.

## Public surface
- `config::Config`, `theme::Theme`
- `keymap::{Action, Keymap}` — `Action` is the public bindable-action
  enum; adding a variant requires a matching arm in
  `sonicterm-app::app::keymap_dispatch`.
- `url_open::validate(url) -> Result<(), Error>`

## Land-mines specific to this crate
- **LM-008** `url_open::validate()` is mandatory before spawning
  anything. OSC 8 URIs come from untrusted PTY output; on Windows
  `cmd /C start` re-tokenizes. Allow-list: `http`, `https`, `mailto`,
  `file`. Deny control chars + `& | ^ < > " ' \` CR LF NUL`. Length
  capped at 4096.
  ref: `crates/sonicterm-cfg/src/url_open.rs`

## Test gate (local)
```bash
cargo test -p sonicterm-cfg
```

## Common pitfalls
- Adding a bindable action without the `keymap_dispatch.rs` arm — runtime no-op
- Loosening the URL allow-list — security regression class
- TOML key rename without a migration shim

## Owning PM(s)
- Primary: either
- Hot-file: keymap.rs is a hot file (parallelism risk)

## Cross-references
- Consumes traits from: `sonicterm-types`
- Consumed by: `sonicterm-app`, `sonicterm-ui`, both platform shells
