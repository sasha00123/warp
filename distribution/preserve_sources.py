#!/usr/bin/env python3
"""Preserve locked dependency sources without merging identically named crates."""
import hashlib
import json
import shutil
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def git(root, *args):
    return subprocess.check_output(["git", "-C", str(root), *args]).decode().strip()


def preserve(packages, output):
    output.mkdir(parents=True, exist_ok=False)
    copied = set()
    index = []
    for package in packages:
        source = package.get("source")
        if source is None:
            continue
        source_id = hashlib.sha256(source.encode()).hexdigest()[:20]
        manifest = Path(package["manifest_path"]).resolve()
        if source.startswith("registry+"):
            relative = Path("registry") / source_id / f'{package["name"]}-{package["version"]}'
            destination = output / relative
            if relative not in copied:
                shutil.copytree(manifest.parent, destination, symlinks=True)
                copied.add(relative)
            manifest_relative = relative / "Cargo.toml"
        elif source.startswith("git+"):
            checkout = Path(git(manifest.parent, "rev-parse", "--show-toplevel")).resolve()
            commit = git(checkout, "rev-parse", "HEAD")
            if commit != source.rsplit("#", 1)[-1]:
                raise ValueError("Git dependency checkout does not match its locked commit")
            relative = Path("git") / source_id
            if relative not in copied:
                subprocess.run(["git", "-C", str(checkout), "diff", "--exit-code", "HEAD"], check=True)
                # Retain the entire tracked workspace, including shared manifests,
                # licenses and submodules, but no Git database or credential cache.
                files = subprocess.check_output([
                    "git", "-C", str(checkout), "ls-files", "--recurse-submodules", "-z"
                ]).decode().split("\0")
                for name in filter(None, files):
                    destination = output / relative / name
                    destination.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copy2(checkout / name, destination, follow_symlinks=False)
                copied.add(relative)
            manifest_relative = relative / manifest.relative_to(checkout)
        else:
            raise ValueError(f"Unsupported dependency source: {source}")
        index.append({"name": package["name"], "version": package["version"],
                      "source": source, "manifest": str(manifest_relative)})
    if not index:
        raise ValueError("No dependency sources were preserved")
    (output / "manifest.json").write_text(json.dumps(index, indent=2) + "\n")
    (output / "README.md").write_text(
        "# Corresponding dependency sources\n\n"
        "manifest.json maps every resolved dependency to its exact source snapshot. "
        "Registry packages retain their Cargo manifests; Git dependencies include "
        "their full tracked workspace at the Cargo.lock commit. No credentials are included.\n\n"
        "These are source snapshots, not a single Cargo directory source: Warp uses "
        "identical crate names and versions from different origins, which cargo vendor "
        "cannot combine. Rebuild the application with its original Cargo.lock and "
        "cargo --locked commands from distribution/README.md. Cargo resolution and "
        "non-Cargo build downloads may still require network access.\n"
    )
    return index


if __name__ == "__main__":
    metadata = json.loads(subprocess.check_output([
        "cargo", "metadata", "--locked", "--all-features", "--format-version", "1"
    ], cwd=ROOT))
    output = ROOT / "target/personal-dependency-sources"
    if output.exists():
        shutil.rmtree(output)
    index = preserve(metadata["packages"], output)
    print(f"Preserved {len(index)} locked dependency packages in {output}")
