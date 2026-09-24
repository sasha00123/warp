#!/usr/bin/env python3
"""Package the exact Linux extension built alongside a custom GUI release."""
import hashlib
import io
import json
import re
import subprocess
import sys
import tarfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def inspect_elf(binary, architecture):
    expected_machine = {"aarch64": 183, "x86_64": 62}.get(architecture)
    if expected_machine is None:
        raise ValueError("unsupported remote architecture")
    with Path(binary).open("rb") as stream:
        header = stream.read(64)
    if (len(header) != 64 or header[:6] != b"\x7fELF\x02\x01"
            or int.from_bytes(header[18:20], "little") != expected_machine):
        raise ValueError("binary is not an ELF64 executable for the requested architecture")


def package(binary, version, architecture):
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
        raise ValueError("version must be numeric major.minor.patch")
    binary = Path(binary).resolve()
    inspect_elf(binary, architecture)
    subprocess.run(["git", "diff", "--exit-code", "HEAD"], cwd=ROOT, check=True)
    commit = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    config = json.loads((ROOT / "distribution/config.json").read_text())
    name = f'{config["cask"]}-{version}-remote-linux-{architecture}.tar.gz'
    with binary.open("rb") as stream:
        binary_hash = hashlib.file_digest(stream, "sha256").hexdigest()
    provenance = {"version": version, "commit": commit, "os": "linux",
                  "architecture": architecture, "asset": name,
                  "repository": config["repository"], "binary_sha256": binary_hash}
    dist = ROOT / "dist"
    dist.mkdir(exist_ok=True)
    archive_path = dist / name
    with tarfile.open(archive_path, "w:gz") as archive:
        archive.add(binary, arcname="warp-oss", recursive=False)
        for license_file in ROOT.glob("LICENSE*"):
            if license_file.is_file():
                archive.add(license_file, arcname=f"licenses/{license_file.name}", recursive=False)
        notice = json.dumps(provenance, indent=2).encode() + b"\n"
        info = tarfile.TarInfo("build.json")
        info.size = len(notice)
        info.mode = 0o644
        archive.addfile(info, io.BytesIO(notice))
    with archive_path.open("rb") as stream:
        provenance["sha256"] = hashlib.file_digest(stream, "sha256").hexdigest()
    (dist / f"manifest-remote-linux-{architecture}.json").write_text(
        json.dumps(provenance, indent=2) + "\n")


if __name__ == "__main__":
    package(*sys.argv[1:])
