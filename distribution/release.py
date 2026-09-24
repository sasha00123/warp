#!/usr/bin/env python3
import hashlib
import json
import re
import subprocess
import sys
import tarfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

def assemble(version, commit):
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version) or not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise ValueError("invalid version or commit")
    config = json.loads((ROOT / "distribution/config.json").read_text())
    dist = ROOT / "dist"
    dist.mkdir(exist_ok=True)
    artifacts = []
    architectures = config.get("macos_architectures", ["arm64", "x86_64"])
    if not architectures or len(set(architectures)) != len(architectures) or any(arch not in ("arm64", "x86_64") for arch in architectures):
        raise ValueError("invalid macOS architecture matrix")
    for arch in architectures:
        manifest = json.loads((dist / f"manifest-{arch}.json").read_text())
        name = f'{config["cask"]}-{version}-macos-{arch}.zip'
        if any(manifest[key] != value for key, value in {
            "version": version, "commit": commit, "architecture": arch,
            "asset": name, "bundle_id": config["bundle_id"], "repository": config["repository"]
        }.items()):
            raise ValueError("artifact provenance mismatch")
        with (dist / name).open("rb") as stream:
            if hashlib.file_digest(stream, "sha256").hexdigest() != manifest["sha256"]:
                raise ValueError("artifact checksum mismatch")
        artifacts.append({"architecture": arch, "asset": name, "sha256": manifest["sha256"]})
    remote_artifacts = []
    for platform in config.get("remote_platforms", []):
        if platform not in ("linux-aarch64", "linux-x86_64"):
            raise ValueError("unsupported remote platform")
        os_name, arch = platform.split("-", 1)
        manifest = json.loads((dist / f"manifest-remote-{platform}.json").read_text())
        name = f'{config["cask"]}-{version}-remote-{platform}.tar.gz'
        expected = {"version": version, "commit": commit, "os": os_name,
                    "architecture": arch, "asset": name, "repository": config["repository"]}
        if any(manifest[key] != value for key, value in expected.items()):
            raise ValueError("remote artifact provenance mismatch")
        with (dist / name).open("rb") as stream:
            if hashlib.file_digest(stream, "sha256").hexdigest() != manifest["sha256"]:
                raise ValueError("remote artifact checksum mismatch")
        remote_artifacts.append({**expected, "sha256": manifest["sha256"]})
    actual_commit = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    if actual_commit != commit:
        raise ValueError("source checkout does not match binary commit")
    subprocess.run(["git", "diff", "--exit-code", "HEAD"], cwd=ROOT, check=True)
    source = dist / f'{config["cask"]}-{version}-source.tar.gz'
    tracked = subprocess.check_output(["git", "ls-files", "--recurse-submodules", "-z"], cwd=ROOT).decode().split("\0")
    # Archive the checkout actually built, including initialized submodule sources.
    prefix = f'{config["cask"]}-{version}-source'
    with tarfile.open(source, "w:gz") as archive:
        for name in tracked:
            if name:
                archive.add(ROOT / name, arcname=f"{prefix}/{name}", recursive=False)
        dependencies = ROOT / "target/personal-dependency-sources"
        if not (dependencies / "manifest.json").is_file():
            raise ValueError("Missing corresponding dependency source snapshots")
        archive.add(dependencies, arcname=f"{prefix}/dependency-sources")
    output = {**config, "version": version, "tag": f"personal-v{version}", "commit": commit, "assets": artifacts, "remote_assets": remote_artifacts,
              "source_asset": source.name, "signing": "ad-hoc; not notarized"}
    (dist / "homebrew.json").write_text(json.dumps(output, indent=2) + "\n")
    files = [source, dist / "homebrew.json"] + [dist / item["asset"] for item in artifacts + remote_artifacts]
    lines = []
    for path in files:
        with path.open("rb") as stream:
            lines.append(f'{hashlib.file_digest(stream, "sha256").hexdigest()}  {path.name}')
    (dist / "SHA256SUMS").write_text("\n".join(lines) + "\n")
    (dist / "RELEASE_NOTES.md").write_text(
        f'# {config["app_name"]} {version}\n\nUnofficial personal build; not affiliated with upstream.\n\n'
        f'Source commit: https://github.com/{config["repository"]}/tree/{commit}\n\n'
        f'Corresponding source and build instructions: `{source.name}`, `distribution/README.md`. '
        'Includes original license notices and locked dependencies.\n\n'
        'Ad-hoc signed, not Developer ID signed or notarized. macOS may require Open Anyway. '
        'No automatic upstream updates. Update via GitHub Releases or the personal Homebrew tap.\n\n'
        'Includes version-matched persistent SSH extensions for the listed Linux architectures. '
        'Maintainer: test every listed artifact in the isolated VM acceptance matrix before publishing.\n')

if __name__ == "__main__":
    assemble(*sys.argv[1:])
