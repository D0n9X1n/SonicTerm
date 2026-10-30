#!/usr/bin/env python3
"""Stage the reviewed x64 ConPTY runtime into an existing destination directory."""

from __future__ import annotations

import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import re
import shutil
import stat
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
import zipfile
import zlib

MANIFEST_PATH = Path(__file__).resolve().with_name("windows-conpty.json")
MEMBERS = {
    "conpty.dll": "runtimes/win-x64/native/conpty.dll",
    "OpenConsole.exe": "build/native/runtimes/x64/OpenConsole.exe",
}
SOCKET_TIMEOUT_S = 15
DOWNLOAD_TIMEOUT_S = 120
CHUNK_SIZE = 65536
MAX_REDIRECTS = 5


class StageError(Exception):
    """An input or verification failure that must publish no runtime."""


def _keys(value, expected, label):
    if not isinstance(value, dict) or set(value) != set(expected):
        raise StageError(f"{label}: unsupported schema keys")


def _pin(record, label):
    if type(record["size"]) is not int or record["size"] <= 0:
        raise StageError(f"{label}: size must be a positive integer")
    if not isinstance(record["sha256"], str) or not re.fullmatch(r"[0-9a-f]{64}", record["sha256"]):
        raise StageError(f"{label}: sha256 must be 64 lowercase hex digits")


def _https(url):
    if not isinstance(url, str):
        raise StageError("URL must use HTTPS")
    parsed = urllib.parse.urlsplit(url)
    if parsed.scheme != "https" or not parsed.hostname or parsed.username or parsed.password or parsed.fragment:
        raise StageError(f"URL must use HTTPS without credentials or fragment: {url!r}")


def _unique_object(pairs):
    document = {}
    for name, value in pairs:
        if name in document:
            raise StageError(f"manifest has duplicate key: {name}")
        document[name] = value
    return document


def load_manifest(path):
    """Validate the fixed package schema before using any path or size from it."""
    document = json.loads(path.read_text(encoding="utf-8"), object_pairs_hook=_unique_object)
    _keys(document, ("schema_version", "package_id", "version", "architecture",
                     "reviewed_upstream_revision", "archive", "files", "license"), "manifest")
    if type(document["schema_version"]) is not int or document["schema_version"] != 1:
        raise StageError("unsupported manifest schema_version")
    if document["package_id"] != "Microsoft.Windows.Console.ConPTY" or document["architecture"] != "x64":
        raise StageError("manifest must pin Microsoft.Windows.Console.ConPTY for x64")
    version = document["version"]
    revision = document["reviewed_upstream_revision"]
    if not isinstance(version, str) or not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
        raise StageError("manifest version must be an exact package version")
    if not isinstance(revision, str) or not re.fullmatch(r"[0-9a-f]{40}", revision):
        raise StageError("manifest reviewed_upstream_revision must be a full revision")
    _keys(document["archive"], ("url", "size", "sha256"), "archive")
    _pin(document["archive"], "archive")
    _https(document["archive"]["url"])
    if document["archive"]["url"] != f"https://www.nuget.org/api/v2/package/Microsoft.Windows.Console.ConPTY/{version}":
        raise StageError("archive URL does not match the pinned package version")
    _keys(document["files"], MEMBERS, "files")
    for name, member in MEMBERS.items():
        record = document["files"][name]
        _keys(record, ("member", "size", "sha256"), name)
        _pin(record, name)
        if record["member"] != member:
            raise StageError(f"{name}: member must be {member}")
    license_record = document["license"]
    _keys(license_record, ("source", "url", "size", "sha256"), "license")
    _pin(license_record, "license")
    if license_record["source"] != "LICENSE-conpty":
        raise StageError("license source must be LICENSE-conpty")
    if license_record["url"] != f"https://raw.githubusercontent.com/microsoft/terminal/{revision}/LICENSE":
        raise StageError("license URL does not match the reviewed upstream revision")
    return document


def _not_link(path, metadata):
    if stat.S_ISLNK(metadata.st_mode) or getattr(metadata, "st_file_attributes", 0) & 0x400:
        raise StageError(f"symlink or reparse point refused: {path}")


def _directory(path):
    for ancestor in (*reversed(path.parents), path):
        try:
            metadata = ancestor.lstat()
        except FileNotFoundError as error:
            raise StageError(f"existing directory required: {ancestor}") from error
        _not_link(ancestor, metadata)
        if not stat.S_ISDIR(metadata.st_mode):
            raise StageError(f"existing real directory required: {ancestor}")


def _verify_bytes(data, record, label):
    if len(data) != record["size"]:
        raise StageError(f"{label}: size mismatch (expected {record['size']}, found {len(data)})")
    if hashlib.sha256(data).hexdigest() != record["sha256"]:
        raise StageError(f"{label}: sha256 mismatch")
    return data


def _read_verified(path, record):
    metadata = path.lstat()
    _not_link(path, metadata)
    if not stat.S_ISREG(metadata.st_mode):
        raise StageError(f"regular file required: {path}")
    if metadata.st_size != record["size"]:
        raise StageError(f"{path}: size mismatch (expected {record['size']}, found {metadata.st_size})")
    with path.open("rb") as source:
        data = source.read(record["size"] + 1)
    return _verify_bytes(data, record, str(path))


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def http_error_302(self, request, response, code, message, headers):
        # urllib's normal redirect handler drains the body without the pinned download cap.
        return response

    http_error_301 = http_error_302
    http_error_303 = http_error_302
    http_error_307 = http_error_302
    http_error_308 = http_error_302


def _remaining(deadline):
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise StageError("download deadline exceeded")
    return min(SOCKET_TIMEOUT_S, remaining)


def _download(record, destination, opener):
    deadline = time.monotonic() + DOWNLOAD_TIMEOUT_S
    url = record["url"]
    if opener is None:
        opener = urllib.request.build_opener(_NoRedirect()).open
    for redirects in range(MAX_REDIRECTS + 1):
        _https(url)
        request = urllib.request.Request(url, headers={"Accept-Encoding": "identity"})
        try:
            opened = opener(request, timeout=_remaining(deadline))
        except urllib.error.HTTPError as error:
            error.close()
            raise StageError(f"download returned HTTP {error.code}") from error
        with opened as response:
            _remaining(deadline)
            _https(response.geturl())
            status = getattr(response, "status", 200)
            if status in (301, 302, 303, 307, 308):
                location = response.headers.get("Location")
                if not location or redirects == MAX_REDIRECTS:
                    raise StageError("download redirect limit exceeded or Location missing")
                url = urllib.parse.urljoin(url, location)
                _https(url)
                continue
            if status != 200:
                raise StageError(f"download returned HTTP {status}")
            content_length = response.headers.get("Content-Length")
            if content_length is not None and (
                not re.fullmatch(r"[0-9]+", content_length) or int(content_length) != record["size"]
            ):
                raise StageError("download Content-Length does not match pinned size")
            received = 0
            with destination.open("xb") as output:
                while True:
                    timeout = _remaining(deadline)
                    # HTTPResponse exposes its socket through the buffered stream; EOF may already close it.
                    stream = getattr(response, "fp", None)
                    socket = getattr(getattr(stream, "raw", None), "_sock", None)
                    if socket is not None:
                        socket.settimeout(timeout)
                    chunk = response.read1(min(CHUNK_SIZE, record["size"] + 1 - received))
                    _remaining(deadline)
                    received += len(chunk)
                    if received > record["size"]:
                        raise StageError("download size exceeds pinned size")
                    if not chunk:
                        break
                    output.write(chunk)
            return _read_verified(destination, record)
    raise StageError("download redirect limit exceeded")


def _payloads(data, records):
    payloads = {}
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        for name, record in records.items():
            matches = [member for member in archive.infolist() if member.filename == record["member"]]
            if len(matches) != 1:
                raise StageError(f"member must occur exactly once: {record['member']}")
            member = matches[0]
            if member.orig_filename != member.filename:
                raise StageError(f"normalized member name refused: {member.orig_filename!r}")
            mode = stat.S_IFMT(member.external_attr >> 16)
            if member.flag_bits & 1 or member.is_dir() or member.external_attr & 0x10 or mode not in (0, stat.S_IFREG):
                raise StageError(f"encrypted or non-regular member refused: {member.filename}")
            if member.compress_type not in (zipfile.ZIP_STORED, zipfile.ZIP_DEFLATED):
                raise StageError(f"unsupported member compression: {member.filename}")
            if member.file_size != record["size"]:
                raise StageError(f"member size mismatch: {member.filename}")
            with archive.open(member) as source:
                payloads[name] = _verify_bytes(source.read(record["size"] + 1), record, f"member {member.filename}")
    return payloads


def _existing(target, document):
    if not os.path.lexists(target):
        return False
    _directory(target)
    records = {**document["files"], "LICENSE": document["license"]}
    if {entry.name for entry in target.iterdir()} != set(records):
        raise StageError(f"{target}: existing runtime has a partial or extra file set; refusing overwrite")
    for name, record in records.items():
        _read_verified(target / name, record)
    return True


def stage(destination, *, archive=None, manifest_path=MANIFEST_PATH, opener=None, temp_root=None):
    """Publish only verified runtime files; an exact existing installation is a no-op."""
    staging = None
    try:
        destination = Path(destination).absolute()
        _directory(destination)
        manifest_path = Path(manifest_path)
        document = load_manifest(manifest_path)
        license_data = _read_verified(manifest_path.with_name("LICENSE-conpty"), document["license"])
        target = destination / "conpty"
        if _existing(target, document):
            return target
        if archive is not None:
            data = _read_verified(Path(archive), document["archive"])
        else:
            with tempfile.TemporaryDirectory(prefix="sonicterm-conpty-", dir=temp_root) as temporary:
                data = _download(document["archive"], Path(temporary) / "package.nupkg", opener)
        payloads = _payloads(data, document["files"])
        payloads["LICENSE"] = license_data
        staging = Path(tempfile.mkdtemp(prefix=".conpty-staging-", dir=destination))
        for name, payload in payloads.items():
            with (staging / name).open("xb") as output:
                output.write(payload)
        _directory(destination)
        # Windows rename refuses an existing target; POSIX has a check/rename race for empty directories.
        if os.path.lexists(target):
            raise StageError(f"{target}: appeared during staging; refusing overwrite")
        staging.rename(target)
        staging = None
        return target
    except (OSError, ValueError, zipfile.BadZipFile, RuntimeError, EOFError, zlib.error) as error:
        raise StageError(str(error)) from error
    finally:
        if staging is not None:
            shutil.rmtree(staging)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--destination", type=Path, required=True, help="existing directory to receive conpty/")
    parser.add_argument("--archive", type=Path, help="verified local .nupkg; disables network access")
    arguments = parser.parse_args(argv)
    try:
        target = stage(arguments.destination, archive=arguments.archive)
    except StageError as error:
        print(f"setup-windows-conpty: {error}", file=sys.stderr)
        return 1
    print(f"setup-windows-conpty: verified {target}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
