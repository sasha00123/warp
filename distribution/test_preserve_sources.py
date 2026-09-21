import json
import subprocess
import tempfile
import unittest
from pathlib import Path

from preserve_sources import preserve


class PreserveSources(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.registry = self.root / "registry-crate"
        self.registry.mkdir()
        (self.registry / "Cargo.toml").write_text('[package]\nname="example"\nversion="1.0.0"\n')
        (self.registry / "lib.rs").write_text("// Registry source\n")
        self.checkout = self.root / "git-checkout"
        self.checkout.mkdir()
        self.git("init", "-b", "main")
        self.git("config", "user.name", "Test")
        self.git("config", "user.email", "test@example.invalid")
        (self.checkout / "LICENSE").write_text("Shared workspace license")
        crate = self.checkout / "member"
        crate.mkdir()
        (crate / "Cargo.toml").write_text('[package]\nname="example"\nversion="1.0.0"\n')
        (crate / "lib.rs").write_text("// Git source\n")
        self.git("add", ".")
        self.git("commit", "-m", "Fixture")
        self.commit = self.git("rev-parse", "HEAD")
        self.packages = [
            {"name": "example", "version": "1.0.0", "source": "registry+https://example.invalid/index",
             "manifest_path": str(self.registry / "Cargo.toml")},
            {"name": "example", "version": "1.0.0", "source": "git+https://example.invalid/repo#" + self.commit,
             "manifest_path": str(crate / "Cargo.toml")},
        ]

    def git(self, *args):
        return subprocess.check_output(["git", "-C", str(self.checkout), *args], stderr=subprocess.STDOUT, text=True).strip()

    def test_duplicate_name_and_version_preserve_both_sources_and_workspace_license(self):
        (self.checkout / "untracked-secret").write_text("Must not be copied")
        output = self.root / "output"
        index = preserve(self.packages, output)
        self.assertEqual(len(index), 2)
        contents = [(output / item["manifest"]).with_name("lib.rs").read_text() for item in index]
        self.assertEqual(contents, ["// Registry source\n", "// Git source\n"])
        git_workspace = (output / index[1]["manifest"]).parent.parent
        self.assertTrue((git_workspace / "LICENSE").is_file())
        self.assertFalse((git_workspace / ".git").exists())
        self.assertFalse((git_workspace / "untracked-secret").exists())
        self.assertEqual(json.loads((output / "manifest.json").read_text()), index)

    def test_rejects_wrong_git_revision(self):
        self.packages[1]["source"] = "git+https://example.invalid/repo#" + "a" * 40
        with self.assertRaisesRegex(ValueError, "locked commit"):
            preserve(self.packages, self.root / "output")

    def test_rejects_modified_git_sources(self):
        (self.checkout / "LICENSE").write_text("Changed after checkout")
        with self.assertRaises(subprocess.CalledProcessError):
            preserve(self.packages, self.root / "output")

    def test_rejects_missing_sources(self):
        with self.assertRaisesRegex(ValueError, "No dependency"):
            preserve([], self.root / "output")


if __name__ == "__main__":
    unittest.main()
