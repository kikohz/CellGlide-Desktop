# Tests for scripts/release-windows-json.py (standard library only).
# Run: python -I -B -m unittest discover -s scripts/tests -p "test_release_*.py"
#  or: python -I -B scripts/tests/test_release_windows_json.py
# (python -I -m unittest scripts/tests/<file>.py does not work: -I keeps the current directory off
# sys.path, so the dotted module name cannot be imported.)
import hashlib, importlib.util, json, subprocess, sys, tempfile, unittest
from unittest import mock
from pathlib import Path
SCRIPT = Path(__file__).resolve().parents[1] / "release-windows-json.py"
spec = importlib.util.spec_from_file_location("release_windows_json", SCRIPT)
rwj = importlib.util.module_from_spec(spec); spec.loader.exec_module(rwj)
BASE = "https://pkg.cellglide.com"

def make_dist(root: Path, arches=("x64", "arm64"), bad_sum_for=None) -> Path:
    for arch in arches:
        name = f"CellGlide-1.0.0-windows-{arch}.zip"
        data = f"zip bytes {arch}".encode()
        (root / name).write_bytes(data)
        digest = "0" * 64 if arch == bad_sum_for else hashlib.sha256(data).hexdigest()
        (root / f"SHA256SUMS.{arch}").write_text(f"{digest}  {name}\n", encoding="utf-8")
    return root

OUTPUTS = {"SHA256SUMS": "a\n", "latest.json": "b\n", "release-windows.json": "c\n"}

class ReleaseWindowsJson(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(); self.dist = Path(self.tmp.name)
    def tearDown(self):
        self.tmp.cleanup()

    def test_both_architectures(self):
        make_dist(self.dist)
        _, latest, _ = rwj.build(self.dist, "1.0.0", "2026-11-05", BASE, True)
        self.assertEqual(latest["version"], "1.0.0")
        self.assertEqual(latest["x64"]["url"], f"{BASE}/windows/1.0.0/CellGlide-1.0.0-windows-x64.zip")
        data = (self.dist / "CellGlide-1.0.0-windows-x64.zip").read_bytes()
        self.assertEqual(latest["x64"]["size"], len(data))
        self.assertEqual(latest["x64"]["sha256"], hashlib.sha256(data).hexdigest())
        self.assertFalse(latest["x64"]["test_build"]); self.assertTrue(latest["arm64"]["test_build"])

    def test_tested_arm64(self):
        make_dist(self.dist)
        _, latest, website = rwj.build(self.dist, "1.0.0", "2026-11-05", BASE, False)
        self.assertFalse(latest["arm64"]["test_build"])
        self.assertEqual([f["test_build"] for f in website["files"]], [False, False])

    def test_website_order_and_commands(self):
        make_dist(self.dist)
        _, _, website = rwj.build(self.dist, "1.0.0", "2026-11-05", BASE, True)
        self.assertEqual([(f["kind"], f["arch"]) for f in website["files"]], [("zip", "x64"), ("zip", "arm64")])
        self.assertEqual(website["install"], "irm https://pkg.cellglide.com/install.ps1 | iex")
        self.assertEqual(website["uninstall"], "irm https://pkg.cellglide.com/uninstall.ps1 | iex")
        self.assertEqual(website["date"], "2026-11-05")
        local = rwj.build(self.dist, "1.0.0", "2026-11-05", "http://127.0.0.1:8770", True)[2]
        self.assertEqual(local["install"], "irm http://127.0.0.1:8770/install.ps1 | iex")

    def test_sums_text(self):
        make_dist(self.dist)
        sums, _, _ = rwj.build(self.dist, "1.0.0", "2026-11-05", BASE, True)
        lines = sums.splitlines()
        self.assertEqual([l.split("  ")[1] for l in lines], ["CellGlide-1.0.0-windows-arm64.zip", "CellGlide-1.0.0-windows-x64.zip"])
        self.assertTrue(sums.endswith("\n"))
        self.assertRegex(lines[0], r"^[0-9a-f]{64}  \S+$")

    def test_missing_arch_fails(self):
        make_dist(self.dist, arches=("x64",))
        with self.assertRaises(rwj.ReleaseDataError) as e:
            rwj.build(self.dist, "1.0.0", "2026-11-05", BASE, True)
        self.assertIn("arm64", str(e.exception))

    def test_hash_mismatch_fails(self):
        make_dist(self.dist, bad_sum_for="x64")
        with self.assertRaises(rwj.ReleaseDataError) as e:
            rwj.build(self.dist, "1.0.0", "2026-11-05", BASE, True)
        self.assertIn("x64", str(e.exception))

    def test_cli_writes_nothing_on_error(self):
        make_dist(self.dist, arches=("x64",))
        run = subprocess.run([sys.executable, "-I", str(SCRIPT), "--dist", str(self.dist), "--version", "1.0.0", "--date", "2026-11-05"],
                             capture_output=True, text=True)
        self.assertEqual(run.returncode, 1)
        for name in ("latest.json", "release-windows.json", "SHA256SUMS"):
            self.assertFalse((self.dist / name).exists(), name)

    def test_cli_writes_all_three(self):
        make_dist(self.dist)
        run = subprocess.run([sys.executable, "-I", str(SCRIPT), "--dist", str(self.dist), "--version", "1.0.0", "--date", "2026-11-05"],
                             capture_output=True, text=True)
        self.assertEqual(run.returncode, 0, run.stderr)
        self.assertEqual(json.loads((self.dist / "latest.json").read_text(encoding="utf-8"))["version"], "1.0.0")
        self.assertTrue((self.dist / "release-windows.json").is_file() and (self.dist / "SHA256SUMS").is_file())

    # ---- Beyond the brief ----

    def test_latest_matches_decision_14(self):
        make_dist(self.dist)
        _, latest, website = rwj.build(self.dist, "1.0.0", "2026-11-05", BASE, True)
        self.assertEqual(list(latest), ["version", "x64", "arm64"])
        for arch in ("x64", "arm64"):
            self.assertEqual(list(latest[arch]), ["url", "sha256", "size", "test_build"])
        self.assertEqual(list(website), ["version", "date", "install", "uninstall", "files"])
        self.assertEqual(list(website["files"][0]), ["kind", "arch", "url", "size", "sha256", "test_build"])
        for f in website["files"]:
            self.assertEqual({k: f[k] for k in ("url", "sha256", "size", "test_build")}, latest[f["arch"]])

    def test_sums_match_the_per_arch_files(self):
        make_dist(self.dist)
        sums, latest, _ = rwj.build(self.dist, "1.0.0", "2026-11-05", BASE, True)
        for arch in ("x64", "arm64"):
            line = (self.dist / f"SHA256SUMS.{arch}").read_text(encoding="utf-8")
            self.assertIn(line, sums)
            self.assertTrue(line.startswith(latest[arch]["sha256"]))
        self.assertEqual(len(sums.splitlines()), 2)

    def test_base_trailing_slash_and_localhost(self):
        make_dist(self.dist)
        _, latest, website = rwj.build(self.dist, "1.0.0", "2026-11-05", "http://localhost:8770/", True)
        self.assertEqual(latest["arm64"]["url"], "http://localhost:8770/windows/1.0.0/CellGlide-1.0.0-windows-arm64.zip")
        self.assertEqual(website["uninstall"], "irm http://localhost:8770/uninstall.ps1 | iex")

    def test_other_bases_are_refused(self):
        make_dist(self.dist)
        for base in ("https://example.com", "http://pkg.cellglide.com", "http://127.0.0.1", "http://10.0.0.1:8770",
                     "https://pkg.cellglide.com.evil.test", "http://127.0.0.1:8770/sub"):
            with self.assertRaises(rwj.ReleaseDataError, msg=base):
                rwj.build(self.dist, "1.0.0", "2026-11-05", base, True)

    def test_bad_version_or_date_is_refused(self):
        make_dist(self.dist)
        with self.assertRaises(rwj.ReleaseDataError):
            rwj.build(self.dist, "1.0", "2026-11-05", BASE, True)
        for date in ("2026-13-01", "05.11.2026", "2026-11-5"):
            with self.assertRaises(rwj.ReleaseDataError, msg=date):
                rwj.build(self.dist, "1.0.0", date, BASE, True)

    def test_missing_sums_file_fails(self):
        make_dist(self.dist)
        (self.dist / "SHA256SUMS.arm64").unlink()
        with self.assertRaises(rwj.ReleaseDataError) as e:
            rwj.build(self.dist, "1.0.0", "2026-11-05", BASE, True)
        self.assertIn("arm64", str(e.exception))
        self.assertIn("SHA256SUMS.arm64", str(e.exception))

    def test_sums_file_must_name_only_its_zip(self):
        make_dist(self.dist)
        sums = self.dist / "SHA256SUMS.x64"
        extra = sums.read_text(encoding="utf-8") + f"{'1' * 64}  other.zip\n"
        sums.write_text(extra, encoding="utf-8")
        with self.assertRaises(rwj.ReleaseDataError) as e:
            rwj.build(self.dist, "1.0.0", "2026-11-05", BASE, True)
        self.assertIn("x64", str(e.exception))
        sums.write_text(f"{'1' * 64}  CellGlide-1.0.0-windows-arm64.zip\n", encoding="utf-8")
        with self.assertRaises(rwj.ReleaseDataError) as e:
            rwj.build(self.dist, "1.0.0", "2026-11-05", BASE, True)
        self.assertIn("x64", str(e.exception))

    def test_star_form_is_accepted(self):
        make_dist(self.dist)
        sums = self.dist / "SHA256SUMS.x64"
        digest, name = sums.read_text(encoding="utf-8").split()
        sums.write_text(f"{digest.upper()} *{name}\r\n", encoding="utf-8")
        text, latest, _ = rwj.build(self.dist, "1.0.0", "2026-11-05", BASE, True)
        self.assertEqual(latest["x64"]["sha256"], digest)
        self.assertIn(f"{digest}  {name}\n", text)

    def test_zip_of_another_version_is_not_used(self):
        make_dist(self.dist)
        (self.dist / "CellGlide-1.0.0-windows-x64.zip").rename(self.dist / "CellGlide-1.0.1-windows-x64.zip")
        with self.assertRaises(rwj.ReleaseDataError) as e:
            rwj.build(self.dist, "1.0.0", "2026-11-05", BASE, True)
        self.assertIn("x64", str(e.exception))

    def test_cli_file_format(self):
        make_dist(self.dist)
        run = subprocess.run([sys.executable, "-I", str(SCRIPT), "--dist", str(self.dist), "--version", "1.0.0",
                              "--date", "2026-11-05", "--arm64-tested"], capture_output=True, text=True)
        self.assertEqual(run.returncode, 0, run.stderr)
        sums, latest, website = rwj.build(self.dist, "1.0.0", "2026-11-05", BASE, False)
        self.assertEqual((self.dist / "SHA256SUMS").read_bytes(), sums.encode("ascii"))
        for name, data in (("latest.json", latest), ("release-windows.json", website)):
            raw = (self.dist / name).read_bytes()
            self.assertEqual(raw, (json.dumps(data, indent=2, ensure_ascii=False) + "\n").encode("utf-8"), name)
            self.assertNotIn(b"\r", raw)
        self.assertFalse(json.loads((self.dist / "latest.json").read_text(encoding="utf-8"))["arm64"]["test_build"])

    def test_cli_replaces_old_output_and_leaves_no_temp_files(self):
        make_dist(self.dist)
        (self.dist / "latest.json").write_text("old", encoding="utf-8")
        run = subprocess.run([sys.executable, "-I", str(SCRIPT), "--dist", str(self.dist), "--version", "1.0.0",
                              "--date", "2026-11-05", "--base", "http://127.0.0.1:8770"], capture_output=True, text=True)
        self.assertEqual(run.returncode, 0, run.stderr)
        self.assertEqual(json.loads((self.dist / "latest.json").read_text(encoding="utf-8"))["x64"]["url"],
                         "http://127.0.0.1:8770/windows/1.0.0/CellGlide-1.0.0-windows-x64.zip")
        self.assertEqual(sorted(p.name for p in self.dist.iterdir() if p.name.endswith(".tmp")), [])

    def test_cli_error_keeps_old_output(self):
        make_dist(self.dist, bad_sum_for="arm64")
        (self.dist / "latest.json").write_text("old", encoding="utf-8")
        run = subprocess.run([sys.executable, "-I", str(SCRIPT), "--dist", str(self.dist), "--version", "1.0.0",
                              "--date", "2026-11-05"], capture_output=True, text=True)
        self.assertEqual(run.returncode, 1)
        self.assertIn("arm64", run.stderr)
        self.assertEqual((self.dist / "latest.json").read_text(encoding="utf-8"), "old")
        self.assertFalse((self.dist / "SHA256SUMS").exists())

    def test_failed_rename_leaves_no_mixed_set(self):
        make_dist(self.dist)
        for name in ("SHA256SUMS", "latest.json", "release-windows.json"):
            (self.dist / name).write_text("old " + name, encoding="utf-8")
        real_replace = rwj.os.replace
        def flaky(src, dst):
            if str(src).endswith(".tmp") and Path(dst).name == "latest.json":
                raise OSError("disk full")
            real_replace(src, dst)
        with mock.patch.object(rwj.os, "replace", side_effect=flaky):
            with self.assertRaises(OSError):
                rwj.write_all(self.dist, OUTPUTS)
        # SHA256SUMS was already replaced when latest.json failed: it is put back, as are the others.
        for name in ("SHA256SUMS", "latest.json", "release-windows.json"):
            self.assertEqual((self.dist / name).read_text(encoding="utf-8"), "old " + name, name)
        self.assertEqual([p.name for p in self.dist.iterdir() if p.suffix in (".tmp", ".bak")], [])

    def test_write_all_without_old_files_fails_clean(self):
        make_dist(self.dist)
        real_replace = rwj.os.replace
        def flaky(src, dst):
            if Path(dst).name == "release-windows.json":
                raise OSError("disk full")
            real_replace(src, dst)
        with mock.patch.object(rwj.os, "replace", side_effect=flaky):
            with self.assertRaises(OSError):
                rwj.write_all(self.dist, OUTPUTS)
        for name in ("SHA256SUMS", "latest.json", "release-windows.json"):
            self.assertFalse((self.dist / name).exists(), name)
        self.assertEqual([p.name for p in self.dist.iterdir() if p.suffix in (".tmp", ".bak")], [])

if __name__ == "__main__":
    unittest.main()
