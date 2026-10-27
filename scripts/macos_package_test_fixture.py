"""Macos package test fixture."""

import contextlib
import importlib.util
import io
import json
import shutil
import os
from pathlib import Path
import plistlib
import subprocess
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location("package_check", Path(__file__).with_name("test-macos-package.py"))
tool = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(tool)


class FlushedStream(io.StringIO):
    flushed = ""

    def flush(self):
        self.flushed = self.getvalue()



class FakeAttachmentGate:
    def __init__(self, root):
        self.root = root.resolve()
        self.state = self.root / "state"
        self.mount = self.state / "mounted"
        self.image = self.root / "fixture image.dmg"
        self.image.write_bytes(b"image fixture")
        self.started = self.now = 1000.0
        self.deadline = self.now + 420
        self.mounted = False
        self.inventory = []
        self.steps = []
        self.queries = 0
        self.late_attach_query = None
        self.attach_result = ("PASS", 0, 0)
        self.detach_result = ("PASS", 0, 0)
        self.after_attach = None
        self.census_payload = None
        self.oversleep = 0
        self.waits = []

    def owned(self, image=None, mount=None):
        return {"image-path": str(image or self.image), "system-entities": [
            {"dev-entry": "/dev/disk2"},
            {"dev-entry": "/dev/disk2s1", "mount-point": str(mount or self.mount)}]}

    def install(self):
        self.mounted = True
        self.inventory = [self.owned()]
        app = self.mount / "SonicTerm.app/Contents"
        app.mkdir(parents=True, exist_ok=True)
        (app / "fixture").write_bytes(b"installed fixture")

    def clear(self):
        self.mounted = False
        self.inventory = []
        app = self.mount / "SonicTerm.app"
        if app.exists():
            shutil.rmtree(app)

    def execute(self, step, index, root, log_dir, environment, *, output_limit_bytes):
        assert output_limit_bytes == 1 << 20
        assert environment.get("HOME") == os.environ.get("HOME")
        assert "NO_COLOR" not in environment
        assert tuple(step.argv[:1]) == ("/usr/bin/hdiutil",)
        self.steps.append(step)
        status, code, leftovers = "PASS", 0, 0
        payload = b""
        operation = step.argv[1]
        if operation == "info":
            assert step.argv[2:] == ("-plist",)
            self.queries += 1
            if self.queries == self.late_attach_query:
                self.install()
            payload = self.census_payload if self.census_payload is not None else plistlib.dumps({"images": self.inventory})
        elif operation == "attach":
            status, code, leftovers = self.attach_result
            if status == "PASS" and code == 0 and leftovers == 0:
                self.install()
            else:
                payload = b"hdiutil: attach failed - Resource temporarily unavailable\n"
            if self.after_attach:
                self.after_attach(self)
        elif operation == "detach":
            assert step.argv == ("/usr/bin/hdiutil", "detach", "/dev/disk2s1")
            status, code, leftovers = self.detach_result
            if status == "PASS" and code == 0:
                self.clear()
            else:
                payload = b"secondary detach failure\n"
        else:
            raise AssertionError(step.argv)
        path = log_dir / f"{index:02d}-{step.id}.log"
        with path.open("xb") as log:
            tool.GATE._write_header(log, step, tool.GATE.launch_argv(step), root)
            log.write(payload)
            return tool.GATE._finish(log, step, path, tool.GATE.time.monotonic(),
                                     status, code, "", leftovers)

    @contextlib.contextmanager
    def active(self):
        original_is_mount = type(self.mount).is_mount
        def is_mount(path):
            return self.mounted if path == self.mount else original_is_mount(path)
        def sleep(seconds):
            self.waits.append(seconds)
            self.now += seconds + self.oversleep
        with contextlib.ExitStack() as stack:
            stack.enter_context(patch.object(tool.GATE, "run_step", side_effect=self.execute))
            stack.enter_context(patch.object(type(self.mount), "is_mount", is_mount))
            stack.enter_context(patch.object(tool, "DEADLINE", self.deadline))
            stack.enter_context(patch.object(tool, "clock", side_effect=lambda: self.now))
            stack.enter_context(patch.object(tool, "sleep", side_effect=sleep))
            stack.enter_context(contextlib.redirect_stderr(io.StringIO()))
            yield self

    def attachment(self):
        self.state.mkdir()
        return tool.DmgAttachment(self.image, self.state)

    def operations(self):
        return [step.argv[1] for step in self.steps]

    def report(self):
        return json.loads((self.state / "attachment-result.json").read_text())

def measurement_app(state: Path) -> Path:
    """Build the smallest app bundle the font measurement can stage, sign, and image."""
    app = state / "original.app"
    fonts = app / "Contents/Resources/assets/fonts"
    fonts.mkdir(parents=True)
    for name in tool.FONTS:
        (fonts / name).write_bytes(name.encode())
    executable = app / "Contents/MacOS/sonicterm-mac"
    executable.parent.mkdir()
    executable.write_bytes(b"same executable")
    (app / "Contents/Info.plist").write_bytes(plistlib.dumps({"ATSApplicationFontsPath": "assets/fonts"}))
    return app


def scripted_hdiutil(created: list, outcomes: list):
    """Return a fake runner that answers each `hdiutil create` with the next scripted outcome."""
    def execute(command, _cwd, timeout, _environment):
        if command[:2] != ["/usr/bin/hdiutil", "create"]:
            return subprocess.CompletedProcess(command, 0, b"", b"")
        created.append((command, timeout))
        code, stderr = outcomes.pop(0)
        if code == 0:
            # A successful create writes an image whose size reflects the staged fonts.
            subject = Path(command[command.index("-srcfolder") + 1])
            Path(command[-1]).write_bytes(b"x" * len(list(subject.rglob("*.ttf"))))
        return subprocess.CompletedProcess(command, code, b"", stderr)
    return execute
