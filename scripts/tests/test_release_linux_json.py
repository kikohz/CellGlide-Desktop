import hashlib
import importlib.util
import tempfile
import unittest
from pathlib import Path

_spec = importlib.util.spec_from_file_location(
    "release_linux_json", Path(__file__).resolve().parents[1] / "release-linux-json.py"
)
mod = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(mod)

BASE = "https://pkg.cellglide.com"


def sha(b: bytes) -> str:
    return hashlib.sha256(b).hexdigest()


def fake(root: Path, pkgver: str, signed_rpm_suffix: bytes = b""):
    dist = root / "dist" / pkgver
    signed = root / "signed.noindex" / pkgver
    dist.mkdir(parents=True)
    signed.mkdir(parents=True)
    sums = {"x86_64": [], "aarch64": []}
    for deb_arch, rpm_arch, key in (("amd64", "x86_64", "x86_64"), ("arm64", "aarch64", "aarch64")):
        deb = f"cellglide_{pkgver}-1_{deb_arch}.deb"
        rpm = f"cellglide-{pkgver}-1.{rpm_arch}.rpm"
        db, rb = f"deb {deb_arch}".encode(), f"rpm {rpm_arch}".encode()
        (dist / deb).write_bytes(db)
        (dist / rpm).write_bytes(rb)
        (signed / rpm).write_bytes(rb + signed_rpm_suffix)
        sums[key] += [f"{sha(db)}  {deb}", f"{sha(rb)}  {rpm}"]
    for key, lines in sums.items():
        (dist / f"SHA256SUMS.{key}").write_text("\n".join(lines) + "\n")
    return dist, signed


class ReleaseLinuxJsonTest(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)
        self.addCleanup(self._tmp.cleanup)

    def test_final_names(self):
        dist, _ = fake(self.root, "1.0.0")
        d = mod.build(self.root, "1.0.0", "2026-10-20", BASE)
        self.assertEqual(d["version"], "1.0.0")
        self.assertEqual(d["date"], "2026-10-20")
        self.assertEqual(
            [f["url"] for f in d["files"]],
            [
                f"{BASE}/apt/pool/main/c/cellglide/cellglide_1.0.0-1_amd64.deb",
                f"{BASE}/apt/pool/main/c/cellglide/cellglide_1.0.0-1_arm64.deb",
                f"{BASE}/rpm/x86_64/cellglide-1.0.0-1.x86_64.rpm",
                f"{BASE}/rpm/aarch64/cellglide-1.0.0-1.aarch64.rpm",
            ],
        )
        self.assertEqual([(f["kind"], f["arch"]) for f in d["files"]],
                         [("deb", "amd64"), ("deb", "arm64"), ("rpm", "x86_64"), ("rpm", "aarch64")])
        deb = (dist / "cellglide_1.0.0-1_amd64.deb").read_bytes()
        self.assertEqual(d["files"][0]["size"], len(deb))
        self.assertEqual(d["files"][0]["sha256"], sha(deb))

    def test_beta_names(self):
        fake(self.root, "1.0.0~beta.2")
        d = mod.build(self.root, "1.0.0~beta.2", "2026-10-16", BASE)
        self.assertEqual(d["version"], "1.0.0-beta.2")
        self.assertEqual(d["files"][2]["url"], f"{BASE}/rpm/x86_64/cellglide-1.0.0~beta.2-1.x86_64.rpm")

    def test_rpm_hash_from_signed(self):
        dist, signed = fake(self.root, "1.0.0", signed_rpm_suffix=b"SIGNATURE")
        d = mod.build(self.root, "1.0.0", "2026-10-20", BASE)
        rpm = signed / "cellglide-1.0.0-1.x86_64.rpm"
        self.assertEqual(d["files"][2]["sha256"], sha(rpm.read_bytes()))
        self.assertEqual(d["files"][2]["size"], rpm.stat().st_size)
        self.assertNotEqual(d["files"][2]["sha256"], sha((dist / rpm.name).read_bytes()))

    def test_missing_arch_fails(self):
        dist, _ = fake(self.root, "1.0.0")
        (dist / "cellglide_1.0.0-1_arm64.deb").unlink()
        with self.assertRaises(mod.ReleaseDataError) as cm:
            mod.build(self.root, "1.0.0", "2026-10-20", BASE)
        self.assertIn("arm64", str(cm.exception))

    def test_two_matches_fails(self):
        dist, _ = fake(self.root, "1.0.0")
        (dist / "cellglide_1.0.0-2_amd64.deb").write_bytes(b"x")
        with self.assertRaises(mod.ReleaseDataError):
            mod.build(self.root, "1.0.0", "2026-10-20", BASE)

    def test_checksum_mismatch_fails(self):
        dist, _ = fake(self.root, "1.0.0")
        p = dist / "SHA256SUMS.x86_64"
        p.write_text(p.read_text().replace(sha(b"deb amd64"), "0" * 64))
        with self.assertRaises(mod.ReleaseDataError) as cm:
            mod.build(self.root, "1.0.0", "2026-10-20", BASE)
        self.assertIn("differs", str(cm.exception))

    def test_custom_signed_dir(self):
        fake(self.root, "1.0.0", signed_rpm_suffix=b"SIG")
        other = self.root / "elsewhere"
        (other / "1.0.0").mkdir(parents=True)
        for f in (self.root / "signed.noindex" / "1.0.0").iterdir():
            (other / "1.0.0" / f.name).write_bytes(f.read_bytes() + b"X")
        (self.root / "signed.noindex" / "1.0.0" / "cellglide-1.0.0-1.x86_64.rpm").unlink()
        d = mod.build(self.root, "1.0.0", "2026-10-20", BASE, other)
        rpm = other / "1.0.0" / "cellglide-1.0.0-1.x86_64.rpm"
        self.assertEqual(d["files"][2]["sha256"], sha(rpm.read_bytes()))

    def test_missing_signed_rpm_fails(self):
        _, signed = fake(self.root, "1.0.0")
        (signed / "cellglide-1.0.0-1.aarch64.rpm").unlink()
        with self.assertRaises(mod.ReleaseDataError) as cm:
            mod.build(self.root, "1.0.0", "2026-10-20", BASE)
        self.assertIn("aarch64", str(cm.exception))

    def test_wrong_version_package_fails(self):
        dist, _ = fake(self.root, "1.0.0")
        (dist / "cellglide_1.0.0-1_arm64.deb").rename(dist / "cellglide_0.9.0-1_arm64.deb")
        with self.assertRaises(mod.ReleaseDataError):
            mod.build(self.root, "1.0.0", "2026-10-20", BASE)

    def test_missing_sums_file_fails(self):
        dist, _ = fake(self.root, "1.0.0")
        (dist / "SHA256SUMS.aarch64").unlink()
        with self.assertRaises(mod.ReleaseDataError) as cm:
            mod.build(self.root, "1.0.0", "2026-10-20", BASE)
        self.assertIn("SHA256SUMS.aarch64", str(cm.exception))

    def test_deb_not_in_sums_fails(self):
        dist, _ = fake(self.root, "1.0.0")
        (dist / "SHA256SUMS.x86_64").write_text("")
        with self.assertRaises(mod.ReleaseDataError) as cm:
            mod.build(self.root, "1.0.0", "2026-10-20", BASE)
        self.assertIn("not listed", str(cm.exception))


if __name__ == "__main__":
    unittest.main()
