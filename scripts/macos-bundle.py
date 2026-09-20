#!/usr/bin/env python3
"""Bundle Homebrew dylib closures and preserve attribution, not a legal assessment."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import sys
import tempfile

LIMIT = 4 * 1024 * 1024
MACHO = {bytes.fromhex(magic) for magic in (
    "feedface", "cefaedfe", "feedfacf", "cffaedfe", "cafebabe", "bebafeca", "cafebabf", "bfbafeca"
)}
LOAD = re.compile(r"\s+(.+) \(compatibility version ([\d.]+), current version ([\d.]+)(?:, [^)]*)?\)")
NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9._+@-]*\Z")


class BundleError(RuntimeError):
    """An incomplete or unsafe bundle cannot be released."""


def run(*args):
    """Run Apple tools with a deadline and disk-backed, size-checked diagnostics."""
    command = [f"/usr/bin/{args[0]}", *map(str, args[1:])]
    environment = {key: value for key, value in os.environ.items() if not key.startswith("DYLD_")}
    environment["LC_ALL"] = "C"
    with tempfile.TemporaryFile() as out, tempfile.TemporaryFile() as err:
        try:
            result = subprocess.run(command, stdout=out, stderr=err, timeout=60,
                                    env=environment, check=False)
        except (OSError, subprocess.TimeoutExpired) as error:
            raise BundleError(f"{args[0]} failed (60 second deadline): {error}") from error
        out.seek(0)
        err.seek(0)
        stdout, stderr = out.read(LIMIT), err.read(LIMIT)
        if result.returncode or len(stdout) >= LIMIT or len(stderr) >= LIMIT:
            raise BundleError(f"{args[0]} failed or exceeded output limit: {stderr[:8192].decode(errors='replace')}")
        return stdout.decode("utf-8")


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def is_system(name):
    # Shared-cache libraries may not exist as files. Only these canonical lexical roots qualify.
    return (name.startswith(("/usr/lib/", "/System/Library/"))
            and not any(part in (".", "..") for part in name.split("/")))


def checked_path(root, relative):
    path = Path(relative)
    if path.is_absolute() or not path.parts or any(part in (".", "..") for part in path.parts):
        raise BundleError(f"unsafe relative path: {relative}")
    target = root / path
    for parent in [target, *target.parents]:
        if parent == root:
            break
        if parent.is_symlink():
            raise BundleError(f"symlink path is unsupported: {parent}")
    if not target.resolve().is_relative_to(root):
        raise BundleError(f"path escapes bundle: {target}")
    return target


def app_paths(app):
    app = Path(app).absolute()
    if app.is_symlink() or not app.is_dir() or app.suffix != ".app":
        raise BundleError(f"expected a real .app directory, not a symlink: {app}")
    app = app.resolve()
    exe = checked_path(app, "Contents/MacOS/sonicterm-mac")
    frameworks = checked_path(app, "Contents/Frameworks")
    resources = checked_path(app, "Contents/Resources")
    if not exe.is_file():
        raise BundleError(f"missing executable: {exe}")
    if exe.stat().st_nlink != 1:
        raise BundleError(f"hardlinked executable would modify its source; copy it first: {exe}")
    return app, exe, frameworks, resources


def read_json(path):
    if path.stat().st_size > LIMIT:
        raise BundleError(f"JSON exceeds {LIMIT} bytes: {path}")
    return json.loads(path.read_text(encoding="utf-8"))


def macos_version(value):
    if not isinstance(value, str) or not re.fullmatch(r"\d+\.\d+(?:\.\d+)?", value):
        raise BundleError(f"invalid macOS minimum deployment version: {value!r}")
    return tuple(map(int, value.split("."))) + (0,) * (3 - len(value.split(".")))


def info_plist(app):
    path = checked_path(app, "Contents/Info.plist")
    if path.stat().st_nlink != 1 or path.stat().st_size > LIMIT:
        raise BundleError(f"unsafe Info.plist size or hardlink: {path}")
    info = plistlib.loads(path.read_bytes())
    macos_version(info["LSMinimumSystemVersion"])
    return path, info


def inspect(path, arch=None, executable=False):
    with path.open("rb") as handle:
        if handle.read(4) not in MACHO:
            raise BundleError(f"not a Mach-O file: {path}")
    arches = run("lipo", "-archs", path).split()
    if (not arches or (executable and (len(arches) != 1 or arches[0] not in ("arm64", "x86_64")))
            or (arch and arch not in arches)):
        raise BundleError(f"architecture mismatch: {path}: {arches}, expected {arch or 'one shipping architecture'}")
    ids = run("otool", "-D", path).splitlines()[1:]
    if len(ids) > 1:
        raise BundleError(f"unsupported multi-architecture install IDs: {path}")
    install_id = ids[0].strip() if ids else None
    if bool(install_id) == executable:
        raise BundleError(f"expected {'executable' if executable else 'dylib install ID'}: {path}")
    loads, identity = [], None
    for line in run("otool", "-L", path).splitlines()[1:]:
        match = LOAD.fullmatch(line)
        if not match:
            raise BundleError(f"unrecognized otool load command: {path}: {line}")
        name, compatibility, current = match.groups()
        entry = dict(name=name, compatibility_version=compatibility, current_version=current)
        if name == install_id and identity is None:
            identity = entry
        else:
            loads.append(entry)
    if install_id and identity is None:
        raise BundleError(f"missing dylib version metadata: {path}")
    commands = run("otool", "-l", path)
    rpaths = re.findall(r"\bcmd LC_RPATH\s+cmdsize \d+\s+path (.+) \(offset \d+\)", commands)
    if commands.count("cmd LC_RPATH") != len(rpaths):
        raise BundleError(f"unrecognized LC_RPATH: {path}")
    minimums = []
    for command in re.split(r"Load command \d+", commands)[1:]:
        if re.search(r"\bcmd LC_BUILD_VERSION\b", command):
            match = re.search(r"platform (?:1|macos)\s+minos ([\d.]+)", command)
            if not match:
                raise BundleError(f"unsupported platform/deployment load command: {path}")
            minimums.append(match[1])
        elif re.search(r"\bcmd LC_VERSION_MIN_MACOSX\b", command):
            match = re.search(r"\bversion ([\d.]+)", command)
            if match:
                minimums.append(match[1])
    if not minimums:
        raise BundleError(f"missing macOS minimum deployment version: {path}")
    return dict(architecture=arches[0], loads=loads, install_name=identity, rpaths=rpaths,
                minimum_macos=max(minimums, key=macos_version))


def expand(name, loader, exe):
    for token, base in (("@loader_path", loader.parent), ("@executable_path", exe.parent)):
        if name == token or name.startswith(token + "/"):
            return base / name[len(token):].lstrip("/")
    if Path(name).is_absolute():
        return Path(name)
    raise BundleError(f"unsupported load path {name!r} in {loader}")


def resolve(name, loader, exe, rpaths):
    candidates = ([directory / name[len("@rpath/"):] for directory in rpaths]
                  if name.startswith("@rpath/") else [expand(name, loader, exe)])
    for candidate in candidates:
        if candidate.is_file():
            real = candidate.resolve(strict=True)
            if real.suffix != ".dylib" or any(part.endswith(".framework") for part in real.parts):
                raise BundleError(f"unsupported non-system dependency (only dylibs): {real}")
            if not NAME.fullmatch(real.name):
                raise BundleError(f"unsafe dylib basename: {real.name}")
            return real
    raise BundleError(f"unresolved load {name!r} in {loader}; searched {candidates}")


def closure(exe, expected_arch):
    first = inspect(exe, expected_arch, executable=True)
    arch = first["architecture"]
    nodes, names, pending = {}, {}, [(exe, [], first)]
    while pending:
        path, inherited, info = pending.pop()
        if path in nodes:
            continue
        if len(nodes) >= 512:
            raise BundleError("dependency closure exceeds 512 Mach-O files")
        if path != exe:
            info = inspect(path, arch)
            if path.name.casefold() in names and names[path.name.casefold()] != path:
                raise BundleError(f"dylib basename collision: {path} and {names[path.name.casefold()]}")
            names[path.name.casefold()] = path
        rpaths = [expand(item, path, exe) for item in info["rpaths"]] + inherited
        nodes[path] = info
        info["resolved"] = {}
        for load in info["loads"]:
            name = load["name"]
            if not is_system(name):
                target = resolve(name, path, exe, rpaths)
                info["resolved"][name] = target
                pending.append((target, rpaths, None))
    return arch, nodes


def attribution(source):
    kegs = [parent for parent in source.parents if parent.parent.parent.name == "Cellar"]
    if len(kegs) != 1:
        raise BundleError(f"non-Homebrew dependency {source}; provide a supported keg with receipt and licenses")
    keg = kegs[0]
    formula, version = keg.parent.name, keg.name
    if not all(NAME.fullmatch(value) for value in (formula, version)):
        raise BundleError(f"unsafe Homebrew formula/version path: {keg}")
    receipt_path = keg / "INSTALL_RECEIPT.json"
    if not receipt_path.is_file():
        raise BundleError(f"missing Homebrew receipt: {receipt_path}; reinstall the matching formula")
    candidates = list(keg.iterdir())
    doc = keg / "share/doc"
    if doc.exists():
        # Never follow directory links into another keg while finding redistribution notices.
        for directory, dirs, files in os.walk(doc, followlinks=False):
            if any((Path(directory) / name).is_symlink() for name in dirs):
                raise BundleError(f"symlink in license tree: {directory}")
            candidates.extend(Path(directory) / name for name in files)
            if len(candidates) > 10000:
                raise BundleError(f"license search exceeds 10000 files: {keg}")
    licenses = sorted(path for path in candidates if path.name.upper().startswith(("COPYING", "LICENSE", "NOTICE", "AUTHORS")) and path.is_file())
    if not any(path.name.upper().startswith(("COPYING", "LICENSE")) for path in licenses):
        raise BundleError(f"missing COPYING/LICENSE in {keg}; supply the matching installed upstream license before packaging")
    metadata = [receipt_path] + sorted(keg.glob("*sbom*.json"))
    for path in licenses + metadata:
        if not path.resolve(strict=True).is_relative_to(keg):
            raise BundleError(f"license/metadata path escapes Homebrew keg: {path}")
        if path.stat().st_size > LIMIT:
            raise BundleError(f"license/metadata exceeds {LIMIT} bytes: {path}")
    receipt = read_json(receipt_path)
    upstream = []
    receipt_source = receipt.get("source", {})
    if receipt_source.get("url"):
        upstream.append(dict(origin="INSTALL_RECEIPT.json", url=receipt_source["url"]))
    for path in metadata[1:]:
        for package in read_json(path).get("packages", []):
            if (package.get("name") == formula and package.get("SPDXID", "").startswith("SPDXRef-Archive-")
                    and package.get("downloadLocation") not in (None, "NOASSERTION", "NONE")):
                upstream.append(dict(origin=path.name, url=package["downloadLocation"], checksums=package.get("checksums", [])))
    return dict(formula=formula, version=version, receipt_source=receipt_source,
                upstream_sources=upstream), keg, licenses, metadata


def bundle_app(app, expected_arch=None, max_minimum="14.0"):
    app, exe, frameworks, resources = app_paths(app)
    if frameworks.exists() and any(frameworks.iterdir()):
        raise BundleError(f"Frameworks must be empty; stage a fresh app: {frameworks}")
    manifest_path = checked_path(app, "Contents/Resources/native-libraries.json")
    licenses_root = checked_path(app, "Contents/Resources/licenses")
    if manifest_path.exists() or licenses_root.exists():
        raise BundleError("native provenance already exists; stage a fresh app")
    arch, nodes = closure(exe, expected_arch)
    plist_path, plist = info_plist(app)
    requested = plist["LSMinimumSystemVersion"]
    required = max([requested] + [node["minimum_macos"] for node in nodes.values()], key=macos_version)
    if macos_version(required) > macos_version(max_minimum):
        raise BundleError(f"native deployment floor {required} exceeds explicit package ceiling {max_minimum}; use compatible libraries")
    # Resolve and validate the complete closure and attribution before editing any Mach-O bytes.
    evidence = {source: attribution(source) for source in nodes if source != exe}
    frameworks.mkdir(parents=True, exist_ok=True)
    resources.mkdir(parents=True, exist_ok=True)
    for source in evidence:
        shutil.copyfile(source, frameworks / source.name)
        (frameworks / source.name).chmod(0o755)
    for source, info in nodes.items():
        destination = exe if source == exe else frameworks / source.name
        prefix = "@executable_path/../Frameworks/" if source == exe else "@loader_path/"
        for old, target in info["resolved"].items():
            run("install_name_tool", "-change", old, prefix + target.name, destination)
        if source != exe:
            run("install_name_tool", "-id", "@loader_path/" + source.name, destination)
        for rpath in info["rpaths"]:
            run("install_name_tool", "-delete_rpath", rpath, destination)
    libraries = []
    for source, (record, keg, licenses, metadata) in sorted(evidence.items()):
        destination = frameworks / source.name
        run("codesign", "--force", "--sign", "-", destination)
        record.update(path=str(destination.relative_to(app)), source_path=str(source),
                      source_sha256=sha256(source), packaged_sha256=sha256(destination),
                      install_name=nodes[source]["install_name"], minimum_macos=nodes[source]["minimum_macos"])
        for key, files in (("licenses", licenses), ("metadata_files", metadata)):
            record[key] = []
            for path in files:
                relative = Path("Contents/Resources/licenses") / f"{record['formula']}-{record['version']}" / path.relative_to(keg)
                target = checked_path(app, relative)
                target.parent.mkdir(parents=True, exist_ok=True)
                if target.exists() and sha256(target) != sha256(path):
                    raise BundleError(f"license destination collision: {target}")
                shutil.copyfile(path, target)
                record[key].append(dict(path=str(relative), sha256=sha256(target)))
        libraries.append(record)
    if macos_version(required) > macos_version(requested):
        print(f"macos-bundle: raising minimum macOS from {requested} to {required} for linked native code")
        plist["LSMinimumSystemVersion"] = required
        plist_path.write_bytes(plistlib.dumps(plist))
    manifest_path.write_text(json.dumps(dict(schema_version=1, architecture=arch, required_macos=required,
                                             libraries=libraries), indent=2) + "\n", encoding="utf-8")
    verify_app(app, expected_arch, max_minimum)


def verify_app(app, expected_arch=None, max_minimum="14.0"):
    app, exe, frameworks, resources = app_paths(app)
    manifest = read_json(checked_path(app, "Contents/Resources/native-libraries.json"))
    if manifest["schema_version"] != 1:
        raise BundleError("unsupported native manifest schema")
    arch = inspect(exe, expected_arch, executable=True)["architecture"]
    if manifest["architecture"] != arch:
        raise BundleError("manifest architecture differs from executable")
    _, plist = info_plist(app)
    declared = macos_version(plist["LSMinimumSystemVersion"])
    if declared > macos_version(max_minimum):
        raise BundleError(f"declared deployment floor exceeds explicit package ceiling {max_minimum}")
    if declared != macos_version(manifest["required_macos"]):
        raise BundleError("manifest minimum deployment version differs from Info.plist")
    registered = {exe}
    for record in manifest["libraries"]:
        path = checked_path(app, record["path"])
        if path.parent != frameworks or path.suffix != ".dylib" or path in registered:
            raise BundleError(f"invalid/duplicate library path: {path}")
        registered.add(path)
        if not path.is_file() or sha256(path) != record["packaged_sha256"]:
            raise BundleError(f"packaged library missing or hash mismatch: {path}")
        if not record["licenses"]:
            raise BundleError(f"missing license provenance: {path}")
        for item in record["licenses"] + record["metadata_files"]:
            target = checked_path(app, item["path"])
            if not target.is_relative_to(resources / "licenses"):
                raise BundleError(f"invalid license/metadata path: {target}")
            if not target.is_file() or sha256(target) != item["sha256"]:
                raise BundleError(f"license/metadata missing or hash mismatch: {target}")
    if set(frameworks.iterdir()) != registered - {exe}:
        raise BundleError("unregistered or missing Frameworks entry")
    for directory, dirs, files in os.walk(app, followlinks=False):
        for name in dirs + files:
            path = checked_path(app, (Path(directory) / name).relative_to(app))
            if path.is_file() and path not in registered:
                with path.open("rb") as handle:
                    if handle.read(4) in MACHO:
                        raise BundleError(f"unregistered Mach-O file: {path}")
    reachable, pending = set(), [exe]
    while pending:
        path = pending.pop()
        if path in reachable:
            continue
        reachable.add(path)
        info = inspect(path, arch, executable=path == exe)
        if macos_version(info["minimum_macos"]) > declared:
            raise BundleError(f"Mach-O minimum deployment version exceeds Info.plist: {path}")
        if info["rpaths"]:
            raise BundleError(f"residual LC_RPATH in {path}")
        if path != exe and info["install_name"]["name"] != "@loader_path/" + path.name:
            raise BundleError(f"non-relative dylib install ID: {path}")
        for load in info["loads"]:
            name = load["name"]
            if is_system(name):
                continue
            prefix = "@executable_path/../Frameworks/" if path == exe else "@loader_path/"
            if not name.startswith(prefix) or not NAME.fullmatch(name[len(prefix):]):
                raise BundleError(f"host or unresolved load path {name!r} in {path}")
            target = resolve(name, path, exe, [])
            if target not in registered:
                raise BundleError(f"load escapes registered bundle: {path}: {name}")
            pending.append(target)
    if reachable != registered:
        raise BundleError("unreachable libraries in native manifest")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("bundle", "verify"))
    parser.add_argument("app", type=Path)
    parser.add_argument("--expected-arch", choices=("arm64", "x86_64"))
    parser.add_argument("--max-minimum-macos", default="14.0")
    args = parser.parse_args(argv)
    try:
        (bundle_app if args.command == "bundle" else verify_app)(args.app, args.expected_arch, args.max_minimum_macos)
    except (BundleError, OSError, ValueError, KeyError, TypeError) as error:
        print(f"macos-bundle: {error}", file=sys.stderr)
        return 1
    print(f"macos-bundle: {args.command} OK: {args.app}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
