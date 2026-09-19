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
    for arch in ("arm64", "x86_64"):
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
        vendor = ROOT / "target/personal-vendor"
        if not vendor.is_dir():
            raise ValueError("Missing vendored dependency sources")
        archive.add(vendor, arcname=f"{prefix}/vendor")
        vendor_config = (ROOT / "target/personal-vendor-config.toml").read_text().replace("target/personal-vendor", "vendor")
        import io
        info = tarfile.TarInfo(f"{prefix}/distribution/vendor-config.toml")
        data = vendor_config.encode()
        info.size = len(data)
        archive.addfile(info, io.BytesIO(data))
    output = {**config, "version": version, "tag": f"personal-v{version}", "commit": commit, "assets": artifacts,
              "source_asset": source.name, "signing": "ad-hoc; not notarized"}
    (dist / "homebrew.json").write_text(json.dumps(output, indent=2) + "\n")
    files = [source, dist / "homebrew.json"] + [dist / item["asset"] for item in artifacts]
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
        'Maintainer: smoke-test both architecture builds before publishing this draft.\n')

if __name__ == "__main__":
    assemble(*sys.argv[1:])
