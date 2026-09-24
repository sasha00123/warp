import importlib.util
import tempfile
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location("package_remote", Path(__file__).with_name("package_remote.py"))
remote = importlib.util.module_from_spec(spec)
spec.loader.exec_module(remote)


class RemoteArchitectureValidation(unittest.TestCase):
    def test_accepts_only_matching_linux_elf64(self):
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "warp-oss"
            header = bytearray(64)
            header[:6] = b"\x7fELF\x02\x01"
            for arch, machine in [("x86_64", 62), ("aarch64", 183)]:
                header[18:20] = machine.to_bytes(2, "little")
                binary.write_bytes(header)
                remote.inspect_elf(binary, arch)
                other = "aarch64" if arch == "x86_64" else "x86_64"
                with self.assertRaises(ValueError):
                    remote.inspect_elf(binary, other)
            binary.write_bytes(b"not an executable")
            with self.assertRaises(ValueError):
                remote.inspect_elf(binary, "aarch64")
            with self.assertRaises(ValueError):
                remote.inspect_elf(binary, "unknown")


if __name__ == "__main__":
    unittest.main()
