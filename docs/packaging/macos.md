# macOS packaging

Packaging: **macOS** · [Windows](windows.md) · [Index](README.md)

The release workflow builds separate Apple Silicon and Intel binaries, then
runs `scripts/bake-icons.sh` and `scripts/make-macos-dmg.sh` from the repository
root. The packaging script assembles `SonicTerm.app`, copies the runtime assets
and bundled fonts, applies an ad-hoc signature, and creates the architecture-
specific DMG in `dist/`.

For a local package, install the build and packaging tools, build the native
release binary, and use a suffix that identifies the host architecture:

```bash
brew install cairo pkg-config create-dmg imagemagick
cargo build --release -p sonicterm-mac
bash scripts/bake-icons.sh

case "$(uname -m)" in
  arm64)  artifact_suffix=mac-aarch64 ;;
  x86_64) artifact_suffix=mac-x86_64 ;;
  *) echo "unsupported architecture: $(uname -m)" >&2; exit 1 ;;
esac

version="$(cargo metadata --no-deps --format-version 1 | \
  python3 -c 'import json,sys; print(json.load(sys.stdin)["packages"][0]["version"])')"
bash scripts/make-macos-dmg.sh \
  target/release/sonicterm-mac \
  "$version" \
  "$artifact_suffix"
```

The app bundle is ad-hoc signed for internal consistency, but it is not signed
with an Apple Developer ID or notarized. A downloaded build can therefore show
the normal unidentified-developer warning; use Finder's **Open** context-menu
action if macOS blocks the first launch.

The release workflow supplies the tag and architecture suffix. Pushing a `v*`
tag is a separate, owner-approved release action; running the packaging script
locally does not publish a release.
