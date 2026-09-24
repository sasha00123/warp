import hashlib
import importlib.util
import json
import tempfile
import subprocess
import tarfile
import unittest
from pathlib import Path
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("personal_release", Path(__file__).with_name("release.py"))
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)

class ReleaseValidation(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        (self.root / "distribution").mkdir()
        (self.root / "dist").mkdir()
        self.config = {"cask": "zed-custom", "bundle_id": "io.sasha00123.ZedCustom", "repository": "sasha00123/zed", "app_name": "Zed Custom"}
        (self.root / "distribution/config.json").write_text(json.dumps(self.config))
        self.commit = "a" * 40
        for arch in ("arm64", "x86_64"):
            name = f"zed-custom-1.2.3-macos-{arch}.zip"
            (self.root / "dist" / name).write_bytes(b"test bundle")
            data = {**self.config, "version": "1.2.3", "commit": self.commit, "architecture": arch, "asset": name,
                    "sha256": hashlib.sha256(b"test bundle").hexdigest()}
            (self.root / f"dist/manifest-{arch}.json").write_text(json.dumps(data))
        self.override = patch.object(release, "ROOT", self.root)
        self.override.start()
        self.addCleanup(self.override.stop)

    def prepare_source(self):
        def git(*args):
            return subprocess.check_output(["git", *args], cwd=self.root, stderr=subprocess.STDOUT, text=True).strip()
        git("init", "-b", "main")
        git("config", "user.name", "Test")
        git("config", "user.email", "test@example.invalid")
        (self.root / "Cargo.lock").write_text("# Locked test dependencies\n")
        git("add", "distribution", "Cargo.lock")
        git("commit", "-m", "Source fixture")
        self.commit = git("rev-parse", "HEAD")
        for path in (self.root / "dist").glob("manifest-*.json"):
            data = json.loads(path.read_text()); data["commit"] = self.commit
            path.write_text(json.dumps(data))
        vendor = self.root / "target/personal-dependency-sources/example"
        vendor.mkdir(parents=True)
        (vendor / "LICENSE").write_text("Dependency license fixture")
        (vendor.parent / "manifest.json").write_text('[{"name":"example"}]\n')

    def test_release_contains_corresponding_source_and_dependencies(self):
        self.prepare_source()
        release.assemble("1.2.3", self.commit)
        source = self.root / "dist/zed-custom-1.2.3-source.tar.gz"
        prefix = "zed-custom-1.2.3-source/"
        with tarfile.open(source) as archive:
            self.assertIn(prefix + "Cargo.lock", archive.getnames())
            self.assertIn(prefix + "dependency-sources/example/LICENSE", archive.getnames())
            self.assertFalse(any("/.git/" in name or "/dist/" in name for name in archive.getnames()))
            manifest = json.load(archive.extractfile(prefix + "dependency-sources/manifest.json"))
            self.assertEqual(manifest[0]["name"], "example")
        manifest = json.loads((self.root / "dist/homebrew.json").read_text())
        self.assertEqual(manifest["commit"], self.commit)
        for line in (self.root / "dist/SHA256SUMS").read_text().splitlines():
            checksum, filename = line.split("  ")
            self.assertEqual(checksum, hashlib.sha256((self.root / "dist" / filename).read_bytes()).hexdigest())

    def test_refuses_dirty_source(self):
        self.prepare_source()
        (self.root / "Cargo.lock").write_text("changed source")
        with self.assertRaises(subprocess.CalledProcessError):
            release.assemble("1.2.3", self.commit)

    def enable_remote(self):
        self.config["remote_platforms"] = ["linux-x86_64"]
        (self.root / "distribution/config.json").write_text(json.dumps(self.config))
        name = "zed-custom-1.2.3-remote-linux-x86_64.tar.gz"
        (self.root / "dist" / name).write_bytes(b"test remote")
        manifest = {"version": "1.2.3", "commit": self.commit, "os": "linux",
                    "architecture": "x86_64", "asset": name, "repository": self.config["repository"],
                    "sha256": hashlib.sha256(b"test remote").hexdigest()}
        (self.root / "dist/manifest-remote-linux-x86_64.json").write_text(json.dumps(manifest))

    def test_includes_matching_remote_artifact_and_checksum(self):
        self.enable_remote()
        self.prepare_source()
        release.assemble("1.2.3", self.commit)
        manifest = json.loads((self.root / "dist/homebrew.json").read_text())
        self.assertEqual(len(manifest["remote_assets"]), 1)
        self.assertEqual(manifest["remote_assets"][0]["commit"], self.commit)
        self.assertIn("remote-linux-x86_64.tar.gz", (self.root / "dist/SHA256SUMS").read_text())

    def test_refuses_missing_required_remote_artifact(self):
        self.enable_remote()
        (self.root / "dist/manifest-remote-linux-x86_64.json").unlink()
        with self.assertRaises(FileNotFoundError):
            release.assemble("1.2.3", self.commit)

    def test_refuses_changed_remote_artifact(self):
        self.enable_remote()
        (self.root / "dist/zed-custom-1.2.3-remote-linux-x86_64.tar.gz").write_bytes(b"wrong")
        with self.assertRaisesRegex(ValueError, "remote artifact checksum"):
            release.assemble("1.2.3", self.commit)

    def test_refuses_remote_from_different_commit(self):
        self.enable_remote()
        path = self.root / "dist/manifest-remote-linux-x86_64.json"
        manifest = json.loads(path.read_text())
        manifest["commit"] = "b" * 40
        path.write_text(json.dumps(manifest))
        with self.assertRaisesRegex(ValueError, "remote artifact provenance"):
            release.assemble("1.2.3", self.commit)

    def test_refuses_missing_architecture(self):
        (self.root / "dist/manifest-x86_64.json").unlink()
        with self.assertRaises(FileNotFoundError):
            release.assemble("1.2.3", self.commit)

    def test_refuses_changed_binary(self):
        (self.root / "dist/zed-custom-1.2.3-macos-arm64.zip").write_bytes(b"different")
        with self.assertRaisesRegex(ValueError, "checksum"):
            release.assemble("1.2.3", self.commit)

    def test_refuses_mixed_commits(self):
        with self.assertRaisesRegex(ValueError, "provenance"):
            release.assemble("1.2.3", "b" * 40)

    def test_refuses_shell_syntax_in_version(self):
        with self.assertRaises(ValueError):
            release.assemble("1.2.3;exit", self.commit)

    def test_refuses_asset_path_traversal(self):
        path = self.root / "dist/manifest-arm64.json"
        data = json.loads(path.read_text()); data["asset"] = "../secret"
        path.write_text(json.dumps(data))
        with self.assertRaisesRegex(ValueError, "provenance"):
            release.assemble("1.2.3", self.commit)

if __name__ == "__main__":
    unittest.main()
