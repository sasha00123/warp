import hashlib
import importlib.util
import json
import tempfile
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
        self.config = {"cask": "sasha-edit", "bundle_id": "io.sasha00123.SashaEdit", "repository": "sasha00123/zed", "app_name": "SashaEdit"}
        (self.root / "distribution/config.json").write_text(json.dumps(self.config))
        self.commit = "a" * 40
        for arch in ("arm64", "x86_64"):
            name = f"sasha-edit-1.2.3-macos-{arch}.zip"
            (self.root / "dist" / name).write_bytes(b"test bundle")
            data = {**self.config, "version": "1.2.3", "commit": self.commit, "architecture": arch, "asset": name,
                    "sha256": hashlib.sha256(b"test bundle").hexdigest()}
            (self.root / f"dist/manifest-{arch}.json").write_text(json.dumps(data))
        self.override = patch.object(release, "ROOT", self.root)
        self.override.start()
        self.addCleanup(self.override.stop)

    def test_refuses_missing_architecture(self):
        (self.root / "dist/manifest-x86_64.json").unlink()
        with self.assertRaises(FileNotFoundError):
            release.assemble("1.2.3", self.commit)

    def test_refuses_changed_binary(self):
        (self.root / "dist/sasha-edit-1.2.3-macos-arm64.zip").write_bytes(b"different")
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
