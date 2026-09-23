#!/usr/bin/env bash
# Run every workspace unit, binary, and integration-test target once.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

status=0
python3 scripts/native-dependencies_tests.py || status=1
python3 scripts/native-dependencies.py check || status=1
python3 scripts/macos-bundle_tests.py || status=1
python3 scripts/test-macos-package_tests.py || status=1

# The preserved dependency is excluded from workspace formatting; its authored tests are not.
rustfmt --check --config-path "$repo_root/rustfmt.toml" \
    crates/sonicterm-winit/src/platform_impl/windows/keyboard_tests.rs || status=1

echo "[workspace-gate] pinned desktop winit unit and integration tests"
cargo test --locked --manifest-path crates/sonicterm-winit/Cargo.toml \
    --target-dir "$repo_root/target" --features serde --lib --tests --no-fail-fast || status=1

echo "[workspace-gate] pinned desktop winit documentation"
RUSTDOCFLAGS="-D warnings" cargo doc --locked --manifest-path crates/sonicterm-winit/Cargo.toml \
    --target-dir "$repo_root/target" --features serde --no-deps --lib || status=1

echo "[workspace-gate] cargo test --workspace --lib --bins --tests --no-fail-fast"
set +e
cargo test --workspace --lib --bins --tests --no-fail-fast
cargo_status=$?
set -e
if [ "$cargo_status" -ne 0 ]; then
    status=1
fi
exit "$status"
