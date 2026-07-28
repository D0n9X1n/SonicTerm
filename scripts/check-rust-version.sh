#!/usr/bin/env bash
#
# Wrapper for `check-rust-version.py`, so the gate lists have one command that
# works on both runners and picks the right interpreter name.
#
# The check is in Python because it reads `cargo metadata`. It lives in its own
# file rather than a heredoc for a measured reason: the heredoc spelling needs
# stdin for the JSON, so the program has to arrive another way, and
# `python /dev/fd/3 3<<'PY'` fails on the Windows runner — Git Bash rewrites
# `/dev/fd/3` into a Windows path and hands it to a native `python3.exe`:
#
#     python3.exe: can't open file 'D:\proc\536\fd\3': [Errno 2] No such file
#
# A repository-relative path to a real `.py` file is the spelling already
# proven on that runner by the other Python gate steps.

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

python_cmd=python3
if ! command -v "$python_cmd" >/dev/null 2>&1; then
    python_cmd=python
fi

exec "$python_cmd" scripts/check-rust-version.py "$@"
