#!/usr/bin/env python3
"""Unpack the pinned macOS 14 Homebrew bottles of Cairo and its runtime libraries on Apple Silicon.

Homebrew's current Cairo bottles for macOS 15 declare a macOS 15.0 deployment floor, which the
Apple Silicon package refuses, since it supports macOS 14. On a macOS 15 runner this unpacks the
`arm64_sonoma` bottles pinned in `macos-native-bottles.json` into a private prefix laid out like
Homebrew's (`Cellar/<formula>/<version>` and `opt/<formula>`), never into Homebrew itself, so no
installed formula or its dependents change. Each download is checked against its pinned size and
SHA-256; the bottles' Homebrew paths are rewritten to the prefix, each rewritten dylib is signed
again, every dylib must declare a minimum macOS no newer than the manifest's ceiling and load only
system or prefix libraries, and `PKG_CONFIG_PATH` is pointed at the prefix for the build.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import platform
import posixpath
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request

MANIFEST_PATH = Path(__file__).resolve().with_name("macos-native-bottles.json")
REGISTRY = "https://ghcr.io/v2/homebrew/core"
TOKEN_URL = "https://ghcr.io/token?scope=repository:homebrew/core/{repository}:pull"
REGISTRY_HOST = "ghcr.io"
SOCKET_TIMEOUT_S = 15
DOWNLOAD_TIMEOUT_S = 300
CHUNK_SIZE = 65536
MAX_REDIRECTS = 5
# Each otool, install_name_tool or codesign command must finish within this many seconds; a hung one
# is killed with its children.
COMMAND_TIMEOUT_S = 900
# Killing what a command left behind must finish within this many seconds, or the setup fails.
REAP_TIMEOUT_S = 10
REDIRECT_CODES = (301, 302, 303, 307, 308)
FORMULA_NAME = re.compile(r"[a-z0-9][a-z0-9+_.@-]*")
VERSION = re.compile(r"[0-9][0-9A-Za-z._]*")
# The private prefix's directory name under the work directory.
PREFIX_NAME = "sonicterm-native"
# A bottle names Homebrew's prefix and Cellar with these placeholders, or, when Homebrew could not
# make it relocatable, with the literal Apple Silicon prefix.
PREFIX_PLACEHOLDER = "@@HOMEBREW_PREFIX@@"
CELLAR_PLACEHOLDER = "@@HOMEBREW_CELLAR@@"
HOMEBREW_PREFIX = "/opt/homebrew"
# Libraries under these roots come with macOS.
SYSTEM_ROOTS = ("/usr/lib/", "/System/Library/")
# The first four bytes of a 64-bit, 32-bit or universal Mach-O file.
MACHO_MAGIC = (b"\xcf\xfa\xed\xfe", b"\xce\xfa\xed\xfe", b"\xca\xfe\xba\xbe")


class SetupError(Exception):
    """An input, download or verification failure that must stop the setup."""


def _version_tuple(text):
    if not re.fullmatch(r"[0-9]+(\.[0-9]+){0,2}", text or ""):
        raise SetupError(f"not a macOS version: {text!r}")
    return tuple(int(part) for part in text.split("."))


def load_manifest(path=MANIFEST_PATH):
    """Read and validate the pinned bottle manifest; formulae come dependencies first."""
    try:
        document = json.loads(Path(path).read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        raise SetupError(f"cannot read manifest {path}: {error}") from error
    expected = {"schema_version", "architecture", "bottle_tag", "max_minimum_macos", "registry", "formulae"}
    if not isinstance(document, dict) or set(document) != expected:
        raise SetupError("manifest: unsupported schema keys")
    if document["schema_version"] != 1:
        raise SetupError("manifest: unsupported schema_version")
    if document["architecture"] != "arm64" or document["bottle_tag"] != "arm64_sonoma":
        raise SetupError("manifest must pin arm64_sonoma bottles for arm64")
    if document["registry"] != REGISTRY:
        raise SetupError(f"manifest registry must be {REGISTRY}")
    _version_tuple(document["max_minimum_macos"])
    formulae = document["formulae"]
    if not isinstance(formulae, list) or not formulae:
        raise SetupError("manifest: formulae must be a nonempty list")
    seen = set()
    record_keys = {"formula", "version", "tag", "size", "sha256", "depends_on"}
    for record in formulae:
        if not isinstance(record, dict) or set(record) != record_keys:
            raise SetupError("formula record: unsupported schema keys")
        name = record["formula"]
        if not isinstance(name, str) or not FORMULA_NAME.fullmatch(name) or name in seen:
            raise SetupError(f"formula name invalid or repeated: {name!r}")
        version, tag = record["version"], record["tag"]
        if not isinstance(version, str) or not VERSION.fullmatch(version):
            raise SetupError(f"{name}: version must be an exact version")
        # A bottle tag is the version, or the version and `-N` for the Nth rebuild.
        if tag != version and not re.fullmatch(re.escape(version) + r"-[1-9][0-9]*", str(tag)):
            raise SetupError(f"{name}: tag must be the version or a rebuild of it")
        if type(record["size"]) is not int or record["size"] <= 0:
            raise SetupError(f"{name}: size must be a positive integer")
        if not isinstance(record["sha256"], str) or not re.fullmatch(r"[0-9a-f]{64}", record["sha256"]):
            raise SetupError(f"{name}: sha256 must be 64 lowercase hex digits")
        depends_on = record["depends_on"]
        if not isinstance(depends_on, list) or any(dependency not in seen for dependency in depends_on):
            raise SetupError(f"{name}: every dependency must be listed before it")
        seen.add(name)
    if formulae[-1]["formula"] != "cairo":
        raise SetupError("manifest must end with cairo")
    return document


def repository(formula):
    """The GitHub Container Registry repository Homebrew publishes `formula`'s bottles under."""
    return formula.replace("@", "/").replace("+", "x")


def bottle_filename(record, bottle_tag):
    """The bottle file name Homebrew reads the formula, version and rebuild from."""
    rebuild = record["tag"][len(record["version"]) + 1:]
    suffix = f".{rebuild}" if rebuild else ""
    return f"{record['formula']}--{record['version']}.{bottle_tag}.bottle{suffix}.tar.gz"


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    """Surface redirects so the registry token is never sent to the storage host they point at."""

    def redirect_request(self, *args, **kwargs):
        return None


def _default_opener():
    return urllib.request.build_opener(_NoRedirect()).open


def _remaining(deadline):
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise SetupError("download deadline exceeded")
    return min(SOCKET_TIMEOUT_S, remaining)


def _token(formula, opener):
    url = TOKEN_URL.format(repository=repository(formula))
    try:
        with opener(urllib.request.Request(url), timeout=SOCKET_TIMEOUT_S) as response:
            token = json.loads(response.read().decode("utf-8")).get("token")
    except (urllib.error.URLError, OSError, ValueError) as error:
        raise SetupError(f"{formula}: registry token request failed: {error}") from error
    if not isinstance(token, str) or not token:
        raise SetupError(f"{formula}: registry returned no token")
    return token


def download(record, destination, opener=None):
    """Download one pinned bottle to `destination`, verifying its size and SHA-256."""
    opener = opener or _default_opener()
    url = f"{REGISTRY}/{repository(record['formula'])}/blobs/sha256:{record['sha256']}"
    headers = {"Authorization": f"Bearer {_token(record['formula'], opener)}", "Accept-Encoding": "identity"}
    deadline = time.monotonic() + DOWNLOAD_TIMEOUT_S
    for _ in range(MAX_REDIRECTS + 1):
        try:
            response = opener(urllib.request.Request(url, headers=headers), timeout=_remaining(deadline))
        except urllib.error.HTTPError as error:
            location = error.headers.get("Location") if error.headers else None
            if error.code not in REDIRECT_CODES or not location:
                raise SetupError(f"{record['formula']}: download returned HTTP {error.code}") from error
            url = urllib.parse.urljoin(url, location)
            if urllib.parse.urlsplit(url).scheme != "https":
                raise SetupError(f"{record['formula']}: redirect left HTTPS")
            if urllib.parse.urlsplit(url).hostname != REGISTRY_HOST:
                # The signed storage URL carries its own authorization; the token stays with ghcr.io.
                headers = {"Accept-Encoding": "identity"}
            continue
        except (urllib.error.URLError, OSError) as error:
            raise SetupError(f"{record['formula']}: download failed: {error}") from error
        digest, written = hashlib.sha256(), 0
        try:
            with response, open(destination, "wb") as output:
                while True:
                    chunk = response.read(CHUNK_SIZE)
                    if not chunk:
                        break
                    written += len(chunk)
                    if written > record["size"]:
                        raise SetupError(f"{record['formula']}: download exceeds pinned size")
                    digest.update(chunk)
                    output.write(chunk)
                    _remaining(deadline)
            if written != record["size"]:
                raise SetupError(f"{record['formula']}: size mismatch (expected {record['size']}, found {written})")
            if digest.hexdigest() != record["sha256"]:
                raise SetupError(f"{record['formula']}: sha256 mismatch")
        except BaseException:
            Path(destination).unlink(missing_ok=True)
            raise
        return Path(destination)
    raise SetupError(f"{record['formula']}: redirect limit exceeded")


def extract_bottle(bottle, cellar, record):
    """Unpack one bottle into `cellar`; every member must lie in `<formula>/<version>/`."""
    keg_root = f"{record['formula']}/{record['version']}"
    with tarfile.open(bottle) as archive:
        members = archive.getmembers()
        for member in members:
            name = PurePosixPath(member.name)
            if name.is_absolute() or ".." in name.parts or not (str(name) == keg_root or str(name).startswith(keg_root + "/")):
                raise SetupError(f"{record['formula']}: bottle member {member.name!r} is outside {keg_root}")
            if not (member.isfile() or member.isdir() or member.issym()):
                raise SetupError(f"{record['formula']}: bottle member {member.name!r} is not a file, directory or symlink")
            if member.issym():
                target = posixpath.normpath(posixpath.join(posixpath.dirname(member.name), member.linkname))
                if member.linkname.startswith("/") or not target.startswith(keg_root + "/"):
                    raise SetupError(f"{record['formula']}: symlink {member.name!r} leaves its keg")
        # Each bottle unpacks alone into a new staging directory, so a chain of its own symlinks can
        # reach at most that directory, never a keg another bottle already filled.
        cellar = Path(cellar)
        cellar.parent.mkdir(parents=True, exist_ok=True)
        stage = Path(tempfile.mkdtemp(prefix=f".{record['formula']}-", dir=cellar.parent)).resolve()
        try:
            staged = stage / record["formula"] / record["version"]
            options = {"filter": "data"} if hasattr(tarfile, "data_filter") else {}
            for member in members:
                # Every write and every link must resolve inside this keg, after earlier links; the
                # keg's own directory entry must sit exactly in the staged `<formula>` directory.
                destination = (stage / member.name).parent.resolve()
                if str(PurePosixPath(member.name)) == keg_root:
                    contained = member.isdir() and destination == staged.parent
                else:
                    contained = destination == staged or staged in destination.parents
                if not contained:
                    raise SetupError(f"{record['formula']}: bottle member {member.name!r} resolves outside its keg")
                try:
                    archive.extract(member, stage, **options)
                except (tarfile.TarError, OSError) as error:
                    # When: tarfile or its data filter refuses the member, the bottle fails like any other refusal.
                    raise SetupError(f"{record['formula']}: bottle member {member.name!r} refused: {error}") from error
                if member.issym():
                    target = (stage / member.name).resolve()
                    if target != staged and staged not in target.parents:
                        raise SetupError(f"{record['formula']}: symlink {member.name!r} resolves outside its keg")
            if [path.name for path in stage.iterdir()] != [record["formula"]] or \
                    [path.name for path in staged.parent.iterdir()] != [record["version"]]:
                raise SetupError(f"{record['formula']}: bottle holds more than its {keg_root} keg")
            keg = cellar / record["formula"] / record["version"]
            if keg.exists():
                raise SetupError(f"{record['formula']}: {keg} already exists")
            keg.parent.mkdir(parents=True, exist_ok=True)
            staged.rename(keg)
        finally:
            # rmtree removes a symlink without following it, so cleanup cannot reach another keg.
            shutil.rmtree(stage, ignore_errors=True)
    return keg


def write_receipt(keg, record, bottle_tag):
    """Record where the keg came from; the packager copies this receipt beside its licenses."""
    receipt = keg / "INSTALL_RECEIPT.json"
    if receipt.exists():
        return
    receipt.write_text(json.dumps({
        "installed_by": "scripts/setup-macos-native.py",
        "poured_from_bottle": True,
        "source": {"tap": "homebrew/core", "spec": "stable", "versions": {"stable": record["version"]}},
        "bottle": {"tag": bottle_tag, "registry_tag": record["tag"], "sha256": record["sha256"],
                   "repository": f"{REGISTRY}/{repository(record['formula'])}"},
    }, indent=2) + "\n", encoding="utf-8")


def relocate_path(path, prefix):
    """Where a path a bottle names points once its kegs live under `prefix` instead of Homebrew's."""
    cellar = f"{prefix}/Cellar"
    for old, new in ((CELLAR_PLACEHOLDER, cellar), (PREFIX_PLACEHOLDER, prefix),
                     (f"{HOMEBREW_PREFIX}/Cellar", cellar), (HOMEBREW_PREFIX, prefix)):
        if path == old or path.startswith(old + "/"):
            return new + path[len(old):]
    return path


def relocate_text(data, prefix):
    """A pkg-config or libtool file's bytes with its Homebrew paths moved under `prefix`."""
    for old, new in ((CELLAR_PLACEHOLDER, f"{prefix}/Cellar"), (PREFIX_PLACEHOLDER, prefix),
                     (f"{HOMEBREW_PREFIX}/Cellar/", f"{prefix}/Cellar/"), (f"{HOMEBREW_PREFIX}/", f"{prefix}/")):
        data = data.replace(old.encode(), new.encode())
    return data


def parse_install_id(otool_d_output):
    """The install name an `otool -D` listing gives, or `None` for a file without one."""
    lines = [line.strip() for line in otool_d_output.splitlines()[1:] if line.strip()]
    return lines[0] if lines else None


def parse_loads(otool_l_output):
    """The library paths an `otool -L` listing names."""
    return [match.group(1) for match in re.finditer(r"(?m)^\s+(\S.*?) \(compatibility version", otool_l_output)]


def parse_rpaths(load_commands):
    """The `LC_RPATH` paths an `otool -l` listing names."""
    return re.findall(r"cmd LC_RPATH\s+cmdsize \d+\s+path (.+?) \(offset \d+\)", load_commands)


def is_macho(path):
    """Whether `path` starts like a Mach-O file."""
    with open(path, "rb") as handle:
        return handle.read(4) in MACHO_MAGIC


def relocate_macho(path, prefix, run):
    """Rewrite one Mach-O file's install name, loads and rpaths under `prefix`, then sign it again.

    Returns whether anything changed.
    """
    arguments = []
    install_id = parse_install_id(run(["otool", "-D", str(path)]))
    if install_id and relocate_path(install_id, prefix) != install_id:
        arguments += ["-id", relocate_path(install_id, prefix)]
    for load in parse_loads(run(["otool", "-L", str(path)])):
        if load != install_id and relocate_path(load, prefix) != load:
            arguments += ["-change", load, relocate_path(load, prefix)]
    for rpath in parse_rpaths(run(["otool", "-l", str(path)])):
        if relocate_path(rpath, prefix) != rpath:
            arguments += ["-rpath", rpath, relocate_path(rpath, prefix)]
    if not arguments:
        return False
    path.chmod(path.stat().st_mode | stat.S_IWUSR)
    run(["install_name_tool", *arguments, str(path)])
    # Rewriting load commands invalidates the signature, and Apple Silicon refuses an unsigned dylib.
    run(["codesign", "--force", "--sign", "-", str(path)])
    return True


def relocate_keg(keg, prefix, run):
    """Move one keg's pkg-config and libtool files and its libraries' Mach-O paths under `prefix`."""
    for path in sorted(keg.rglob("*")):
        if path.is_symlink() or not path.is_file():
            continue
        if path.suffix in (".pc", ".la"):
            data = path.read_bytes()
            moved = relocate_text(data, prefix)
            if moved != data:
                path.chmod(path.stat().st_mode | stat.S_IWUSR)
                path.write_bytes(moved)
        elif keg / "lib" in path.parents and is_macho(path):
            relocate_macho(path, prefix, run)


def load_stays_pinned(load, prefix):
    """Whether a library a pinned dylib loads is a system library, relative, or a file in `prefix`."""
    if "@@HOMEBREW" in load:
        return False
    if load.startswith("@") or load.startswith(SYSTEM_ROOTS):
        return True
    return load.startswith(f"{prefix}/") and Path(load).is_file()


def pkg_config_path(manifest, prefix, existing=None):
    """`PKG_CONFIG_PATH` with each pinned keg's pkg-config directories ahead of `existing`."""
    directories = [str(prefix / "opt" / record["formula"] / part / "pkgconfig")
                   for record in manifest["formulae"] for part in ("lib", "share")
                   if (prefix / "opt" / record["formula"] / part / "pkgconfig").is_dir()]
    return os.pathsep.join(directories + ([existing] if existing else []))


def parse_minimum_macos(otool_output):
    """The minimum macOS an `otool -l` listing declares, or `None` when it declares none."""
    match = re.search(r"cmd LC_BUILD_VERSION\b.*?platform (?:1|macos)\s+minos ([0-9.]+)", otool_output, re.S)
    if not match:
        match = re.search(r"cmd LC_VERSION_MIN_MACOSX\b.*?version ([0-9.]+)", otool_output, re.S)
    return match.group(1) if match else None


def _reap_session(session_id, table=None):
    """SIGKILL every process left in `session_id` until a complete census finds none, within `REAP_TIMEOUT_S`."""
    # The process table lives beside this script; it is imported here so loading the module needs only the stdlib.
    if str(MANIFEST_PATH.parent) not in sys.path:
        sys.path.insert(0, str(MANIFEST_PATH.parent))
    import perf_processes
    import perf_sessions
    table = table or perf_processes.make_process_table()
    deadline = time.monotonic() + REAP_TIMEOUT_S
    while True:
        # None means the census is incomplete (the list or a process could not be read), never that it is empty.
        members = perf_sessions.session_members(table, session_id)
        if members == []:
            return
        if time.monotonic() > deadline:
            found = "an incomplete process census" if members is None else [info.pid for info in members]
            raise SetupError(f"session {session_id} could not be shown empty: {found}")
        for info in members or []:
            perf_processes.send_kill(info.pid)
        time.sleep(0.05)


def _run(command, timeout_s=COMMAND_TIMEOUT_S):
    """Run one command in its own session and reap the whole session, killing it after `timeout_s`.

    A tool can start helpers in their own process groups, so the session, not the group, is what
    holds every descendant. Output goes to files, so a surviving helper cannot hold a pipe open.
    """
    environment = dict(os.environ)
    with tempfile.TemporaryFile() as stdout_file, tempfile.TemporaryFile() as stderr_file:
        process = subprocess.Popen(command, stdout=stdout_file, stderr=stderr_file, env=environment,
                                   start_new_session=True)
        try:
            process.wait(timeout=timeout_s)
            timed_out = False
        except subprocess.TimeoutExpired:
            timed_out = True
        # The leader's pid is the session id start_new_session gave it; helpers left behind are killed too.
        try:
            _reap_session(process.pid)
        finally:
            process.kill()
            process.wait()
        stdout_file.seek(0)
        stderr_file.seek(0)
        stdout = stdout_file.read().decode("utf-8", errors="replace")
        stderr = stderr_file.read().decode("utf-8", errors="replace")
    if timed_out:
        raise SetupError(f"{' '.join(command)} timed out after {timeout_s} seconds")
    if process.returncode != 0:
        raise SetupError(f"{' '.join(command)} failed ({process.returncode}): {stderr.strip()[-2000:]}")
    return stdout


def verify(manifest, prefix, run=_run):
    """Require each pinned keg in `prefix`, and every dylib within the ceiling and loading only pinned
    or system libraries; returns the number of dylibs checked."""
    ceiling = _version_tuple(manifest["max_minimum_macos"])
    checked = 0
    for record in manifest["formulae"]:
        keg = prefix / "Cellar" / record["formula"] / record["version"]
        link = prefix / "opt" / record["formula"]
        if not keg.is_dir() or not link.is_symlink() or link.resolve() != keg.resolve():
            raise SetupError(f"{record['formula']}: {link} does not lead to the pinned keg {keg}")
        library = keg / "lib"
        # Some formulae keep dylibs in subdirectories, such as Cairo's `lib/cairo/libcairo-trace.dylib`.
        for dylib in sorted(library.rglob("*.dylib")) if library.is_dir() else []:
            if dylib.is_symlink():
                continue
            minimum = parse_minimum_macos(run(["otool", "-l", str(dylib)]))
            if minimum is None or _version_tuple(minimum) > ceiling:
                raise SetupError(f"{dylib}: minimum macOS {minimum} exceeds {manifest['max_minimum_macos']}")
            install_id = parse_install_id(run(["otool", "-D", str(dylib)]))
            for load in [install_id, *parse_loads(run(["otool", "-L", str(dylib)]))]:
                if load and not load_stays_pinned(load, str(prefix)):
                    raise SetupError(f"{dylib}: {load} is neither a system library nor a pinned one")
            checked += 1
    return checked


def setup(manifest_path=MANIFEST_PATH, work_dir=None, opener=None, run=_run):
    """Download, unpack, relocate and verify every pinned bottle in a new private prefix.

    Returns the prefix and the number of dylibs checked.
    """
    manifest = load_manifest(manifest_path)
    prefix = (Path(work_dir) if work_dir else Path(tempfile.gettempdir())).resolve() / PREFIX_NAME
    if prefix.exists():
        raise SetupError(f"{prefix} already exists; the pinned prefix is built once, into a new directory")
    (prefix / "opt").mkdir(parents=True)
    (prefix / "Cellar").mkdir()
    with tempfile.TemporaryDirectory(dir=work_dir) as directory:
        for record in manifest["formulae"]:
            bottle = download(record, Path(directory) / bottle_filename(record, manifest["bottle_tag"]), opener)
            extract_bottle(bottle, prefix / "Cellar", record)
    for record in manifest["formulae"]:
        keg = prefix / "Cellar" / record["formula"] / record["version"]
        (prefix / "opt" / record["formula"]).symlink_to(keg)
        write_receipt(keg, record, manifest["bottle_tag"])
        relocate_keg(keg, str(prefix), run)
    return prefix, verify(manifest, prefix, run)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--manifest", type=Path, default=MANIFEST_PATH)
    parser.add_argument("--work-dir", type=Path, default=None)
    args = parser.parse_args(argv)
    try:
        if sys.platform != "darwin" or platform.machine() != "arm64":
            raise SetupError("the pinned bottles are for macOS on Apple Silicon only")
        manifest = load_manifest(args.manifest)
        prefix, checked = setup(args.manifest, args.work_dir)
    except SetupError as error:
        print(f"setup-macos-native: {error}", file=sys.stderr)
        return 1
    search = pkg_config_path(manifest, prefix, os.environ.get("PKG_CONFIG_PATH"))
    # Later workflow steps read GITHUB_ENV; a local run prints the value to export instead.
    if os.environ.get("GITHUB_ENV"):
        with open(os.environ["GITHUB_ENV"], "a", encoding="utf-8") as output:
            output.write(f"PKG_CONFIG_PATH={search}\n")
    print(f"setup-macos-native: unpacked the pinned bottles into {prefix}; {checked} dylibs within macOS "
          f"{manifest['max_minimum_macos']}\nPKG_CONFIG_PATH={search}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
