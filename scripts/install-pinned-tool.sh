#!/usr/bin/env bash
# Install one pinned release binary that scripts/pinned-tools.json names, and nothing else.
#
# The only input is the tool name. The version, the release URL, the archive member and the
# SHA-256 come from the manifest beside this script, never from the environment or the
# arguments, so a workflow cannot point the install at another file. The archive is downloaded
# into a fresh directory, that exact file is checked against the pinned SHA-256, and only then
# is it extracted and the binary installed into Cargo's bin directory. Any refusal or mismatch
# exits nonzero and installs nothing.
#
# The download accepts https, and file only so the tests can serve a release from disk; the
# workflow supply-chain checker requires every committed manifest URL to be an https release URL.

set -euo pipefail

usage() {
  printf 'usage: %s <tool>\n' "$0" >&2
  exit 2
}

fail() {
  printf 'install-pinned-tool: %s\n' "$1" >&2
  exit 1
}

if [ "$#" -ne 1 ]; then
  usage
fi
tool="$1"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
manifest="$here/pinned-tools.json"
[ -f "$manifest" ] || fail "no manifest at $manifest"

# The manifest's spelling of this host, so an entry pinned for another platform is refused.
case "$(uname -s)" in
  Darwin) host_os=macos ;;
  Linux) host_os=linux ;;
  *) host_os="$(uname -s)" ;;
esac
case "$(uname -m)" in
  arm64 | aarch64) host_arch=aarch64 ;;
  x86_64 | amd64) host_arch=x86_64 ;;
  *) host_arch="$(uname -m)" ;;
esac

# One field per line: version, digest, os, arch, url, member, version output, then each
# version-check argument. The reader refuses an unknown tool and malformed pins. An entry pins
# one archive at its top level, or one per host in `platforms`, from which it takes this host's.
fields="$(python3 - "$manifest" "$tool" "$host_os" "$host_arch" <<'PY'
import json
import re
import sys

manifest_path, tool, host_os, host_arch = sys.argv[1:5]
with open(manifest_path, encoding="utf-8") as handle:
    tools = json.load(handle)
entry = tools.get(tool) if isinstance(tools, dict) else None
if not isinstance(entry, dict):
    sys.exit(f"install-pinned-tool: {tool} is not in {manifest_path}")
version = entry.get("version")
if not isinstance(version, str) or not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
    sys.exit(f"install-pinned-tool: {tool} version {version!r} is not an exact X.Y.Z")
platforms = entry.get("platforms")
if platforms is None:
    pin = entry
else:
    if not isinstance(platforms, list) or not all(isinstance(candidate, dict) for candidate in platforms):
        sys.exit(f"install-pinned-tool: {tool} platforms must be a list of pins")
    matches = [candidate for candidate in platforms
               if (candidate.get("os"), candidate.get("arch")) == (host_os, host_arch)]
    if len(matches) != 1:
        sys.exit(f"install-pinned-tool: {tool} has no single pin for {host_os}/{host_arch}")
    pin = matches[0]
digest = pin.get("sha256")
if not isinstance(digest, str) or not re.fullmatch(r"[0-9a-f]{64}", digest):
    sys.exit(f"install-pinned-tool: {tool} sha256 must be 64 lowercase hex digits")
lines = [version, digest]
for key, source in (("os", pin), ("arch", pin), ("url", pin), ("member", pin), ("version_output", entry)):
    value = source.get(key)
    if not isinstance(value, str) or not value or "\n" in value:
        sys.exit(f"install-pinned-tool: {tool} has no usable {key}")
    lines.append(value.replace("{version}", version))
arguments = entry.get("version_args")
if not isinstance(arguments, list) or not arguments or not all(
    isinstance(argument, str) and argument and "\n" not in argument for argument in arguments
):
    sys.exit(f"install-pinned-tool: {tool} has no usable version_args")
first_line = entry.get("version_first_line", False)
if not isinstance(first_line, bool):
    sys.exit(f"install-pinned-tool: {tool} version_first_line must be boolean")
lines.append("true" if first_line else "false")
lines.extend(arguments)
print("\n".join(lines))
PY
)"

check_args=()
{
  IFS= read -r version
  IFS= read -r digest
  IFS= read -r pinned_os
  IFS= read -r pinned_arch
  IFS= read -r url
  IFS= read -r member
  IFS= read -r version_output
  IFS= read -r version_first_line
  while IFS= read -r argument; do
    check_args+=("$argument")
  done
} <<<"$fields"

[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "$tool version $version is not an exact X.Y.Z"
[[ "$digest" =~ ^[0-9a-f]{64}$ ]] || fail "$tool sha256 must be 64 lowercase hex digits"
[ "${#check_args[@]}" -gt 0 ] || fail "$tool has no version_args"
if [ "$pinned_os" != "$host_os" ] || [ "$pinned_arch" != "$host_arch" ]; then
  fail "$tool is pinned for $pinned_os/$pinned_arch, not $host_os/$host_arch"
fi

cargo_bin="${CARGO_HOME:-$HOME/.cargo}/bin"
workdir="$(mktemp -d "${TMPDIR:-/tmp}/install-pinned-tool.XXXXXX")"
trap 'rm -rf "$workdir"' EXIT
archive="$workdir/release.tar.gz"

curl --fail --silent --show-error --location --proto '=https,file' --proto-redir '=https' --tlsv1.2 \
  --output "$archive" "$url"

# macOS's sha256sum has no --check; shasum checks the same digest there.
if [ "$host_os" = linux ]; then
  checker=(sha256sum --check --strict)
else
  checker=(shasum -a 256 --check --strict)
fi
if ! printf '%s  %s\n' "$digest" "$archive" | "${checker[@]}" >/dev/null; then
  fail "the download of $tool does not match its pinned SHA-256"
fi

# Only the verified file is extracted, and only the pinned member of it.
mkdir "$workdir/unpacked"
tar -xzf "$archive" -C "$workdir/unpacked" "$member"
mkdir -p "$cargo_bin"
install -m 0755 "$workdir/unpacked/$member" "$cargo_bin/$tool"

if ! reported="$("$cargo_bin/$tool" "${check_args[@]}" 2>&1)"; then
  rm -f "$cargo_bin/$tool"
  fail "$tool could not report its version"
fi
# actionlint follows its version with build metadata; exact first-line matching remains version-pinned.
if [ "$version_first_line" = true ]; then
  reported="${reported%%$'\n'*}"
fi
if [ "$reported" != "$version_output" ]; then
  rm -f "$cargo_bin/$tool"
  fail "$tool reports '$reported', not '$version_output'"
fi
printf 'install-pinned-tool: installed %s at %s\n' "$version_output" "$cargo_bin/$tool"
