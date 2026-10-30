"""Pin staged runtime use before packaged Windows smoke, without compiling or installing an MSI."""
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parent.parent


class PackagedRuntimeTests(unittest.TestCase):
    def test_ci_and_release_stage_before_smoke_and_require_the_package(self):
        # A missing runtime cannot silently fall through to system ConPTY in a shipping smoke.
        for workflow, target in (("ci.yml", "target/release"), ("release.yml", "target/x86_64-pc-windows-msvc/release")):
            source = (ROOT / ".github/workflows" / workflow).read_text(encoding="utf-8")
            stage = source.index(f"python scripts/setup-windows-conpty.py --destination {target}")
            smoke = source.index("      - name: Require Windows native runtime smoke", stage)
            check = source.index(f"python scripts/verify-windows-conpty.py --runtime-dir {target}/conpty", smoke)
            self.assertLess(stage, smoke)
            self.assertLess(smoke, check)
            first = source[smoke:].split("\n      - name:", 1)[0]
            # The selection record is INFO; the default WARN filter would hide it from the verifier.
            self.assertIn("RUST_LOG: warn,sonicterm_io::conpty_runtime=info", first)
            for title in ("Require Windows native runtime smoke", "Require Windows GPU frame-validation smoke", "Require Windows GPU device-recovery smoke"):
                block = source[source.index(f"      - name: {title}", stage):].split("\n      - name:", 1)[0]
                self.assertIn('SONICTERM_REQUIRE_CONPTY: "1"', block)
                self.assertIn(f"{target}/sonicterm-windows.exe --runtime-smoke", block)

    def test_runtime_validation_precedes_the_only_production_open(self):
        # All shell and direct-command paths converge on spawn_built before the upstream loader is initialized.
        source = (ROOT / "crates/sonicterm-io/src/pty_spawn.rs").read_text(encoding="utf-8")
        body = source.split("    fn spawn_built(", 1)[1]
        self.assertLess(body.index("crate::conpty_runtime::ensure_ready()?"), body.index("native_pty_system()"))


if __name__ == "__main__":
    unittest.main()
