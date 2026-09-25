#!/usr/bin/env bash
# Compile and lint Windows-target Rust from a non-Windows host where no Windows C
# toolchain is needed. An optional pre-push aid: it runs nothing, and Windows CI
# remains the only place Windows code executes.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

target="x86_64-pc-windows-msvc"

# Members that lint cleanly for the target without Windows CRT or SDK headers.
verified=(
    sonicterm-types
    sonicterm-grid
    sonicterm-vt
    sonicterm-cfg
    sonicterm-logging
    sonicterm-resource
    sonicterm-text
    sonicterm-ui
    sonicterm-app-core
    sonicterm-io
    sonicterm-render-model
    sonicterm-block-glyph
    sonicterm-font-config
)

# Every other member, as "name|reason". Each reason names the build step or
# dependency that needs a Windows C toolchain or an unverified native closure.
excluded=(
    "sonicterm-freetype|build.rs compiles vendored zlib, libpng, and FreeType with cc, which needs Windows CRT headers"
    "sonicterm-harfbuzz|build.rs compiles vendored HarfBuzz as C++ with cc"
    "sonicterm-fontconfig|build.rs discovers the system Fontconfig through pkg-config"
    "sonicterm-font|links the system Cairo through cairo-rs and depends on sonicterm-freetype and sonicterm-harfbuzz"
    "sonicterm-engine|depends on sonicterm-font, whose native font and Cairo closure is unverified for the target"
    "sonicterm-gpu|depends on sonicterm-engine, whose native font and Cairo closure is unverified for the target"
    "sonicterm-app|depends on sonicterm-gpu, whose native font and Cairo closure is unverified for the target"
    "sonicterm-mac|depends on sonicterm-app, whose native font and Cairo closure is unverified for the target"
    "sonicterm-windows|depends on sonicterm-app, whose native font and Cairo closure is unverified for the target"
    "sonicterm-linux|depends on sonicterm-app and sonicterm-engine, whose native font and Cairo closure is unverified for the target"
)

status=0

members="$(cargo metadata --no-deps --format-version 1 | python3 -c '
import json, sys
metadata = json.load(sys.stdin)
members = set(metadata["workspace_members"])
for package in metadata["packages"]:
    if package["id"] in members:
        print(package["name"])
')" || {
    echo "[windows-target] cargo metadata failed; cannot classify workspace members" >&2
    exit 1
}

excluded_names=()
for entry in "${excluded[@]}"; do
    excluded_names+=("${entry%%|*}")
done

# Every member must be in exactly one list, and every listed name must still be
# a member, so a new or renamed crate cannot fall out of the check unnoticed.
echo "[windows-target] classifying workspace members"
if ! printf '%s\n' "$members" | python3 -c '
import sys
verified = set(sys.argv[1].split())
excluded = set(sys.argv[2].split())
members = {line.strip() for line in sys.stdin if line.strip()}
problems = []
for name in sorted(members - verified - excluded):
    problems.append(f"workspace member {name} is in neither the verified nor the excluded list")
for name in sorted(verified & excluded):
    problems.append(f"{name} is in both the verified and the excluded list")
for name in sorted((verified | excluded) - members):
    problems.append(f"{name} is listed but is not a workspace member")
for problem in problems:
    print(f"[windows-target] {problem}", file=sys.stderr)
sys.exit(1 if problems else 0)
' "${verified[*]}" "${excluded_names[*]}"; then
    status=1
fi

sysroot="$(rustc --print sysroot)"
if [ ! -d "$sysroot/lib/rustlib/$target/lib" ]; then
    echo "[windows-target] the $target standard library is not installed" >&2
    echo "[windows-target] install it with: rustup target add $target" >&2
    exit 1
fi

echo "[windows-target] scope: default features, all targets, and the $target dev and build"
echo "[windows-target] closure of ${#verified[@]} members; nothing runs. Not checked:"
for entry in "${excluded[@]}"; do
    echo "[windows-target]   ${entry%%|*}: ${entry#*|}"
done

# Separate target directories keep Windows-target artifacts out of the host
# build cache and keep the pinned winit's own lockfile out of the workspace's.
check_root="${CARGO_TARGET_DIR:-$repo_root/target}/check-windows-target"

package_args=()
for name in "${verified[@]}"; do
    package_args+=(-p "$name")
done

echo "[windows-target] cargo clippy --locked --target $target for ${#verified[@]} members"
cargo clippy --locked --target "$target" --target-dir "$check_root/workspace" \
    "${package_args[@]}" --all-targets -- -D warnings || status=1

echo "[windows-target] pinned winit library and Windows test targets"
cargo check --locked --manifest-path crates/sonicterm-winit/Cargo.toml --target "$target" \
    --target-dir "$check_root/winit" --features serde --lib --tests || status=1

exit "$status"
