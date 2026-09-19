#!/usr/bin/env python3
import hashlib
import json
import plistlib
import re
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CONFIG = json.loads((ROOT / "distribution/config.json").read_text())

def run(*args):
    subprocess.run(args, check=True, cwd=ROOT)

def package(app, version, arch):
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
        raise ValueError("version must be numeric major.minor.patch")
    if arch not in ("arm64", "x86_64"):
        raise ValueError("unsupported architecture")
    source = Path(app).resolve()
    if not source.is_dir() or source.suffix != ".app":
        raise ValueError("expected an existing app bundle")
    stage = ROOT / "target/personal-stage"
    stage.mkdir(parents=True, exist_ok=True)
    target = stage / (CONFIG["app_name"] + ".app")
    if target.exists():
        shutil.rmtree(target)
    run("ditto", str(source), str(target))
    plist_path = target / "Contents/Info.plist"
    with plist_path.open("rb") as stream:
        plist = plistlib.load(stream)
    # Drop the upstream signing identity and update endpoints before signing.
    for name in list(plist):
        if name.startswith("SU") or name in ("CFBundleIconName", "KSChannelID", "KSProductID", "KSUpdateURL"):
            del plist[name]
    plist.update(CFBundleIdentifier=CONFIG["bundle_id"], CFBundleName=CONFIG["app_name"],
                 CFBundleDisplayName=CONFIG["app_name"], CFBundleVersion=version,
                 CFBundleShortVersionString=version, CFBundleIconFile="Personal.icns",
                 LSMinimumSystemVersion=CONFIG["minimum_macos"],
                 CFBundleURLTypes=[{"CFBundleURLName": CONFIG["app_name"], "CFBundleURLSchemes": [CONFIG["cask"]]}])
    with plist_path.open("wb") as stream:
        plistlib.dump(plist, stream)
    (target / "Contents/embedded.provisionprofile").unlink(missing_ok=True)
    resources = target / "Contents/Resources"
    resources.mkdir(exist_ok=True)
    # This source-drawn icon contains no upstream logo.
    iconset = stage / "Personal.iconset"
    iconset.mkdir(exist_ok=True)
    png = stage / "personal.png"
    sdk = subprocess.check_output(["xcrun", "--sdk", "macosx", "--show-sdk-path"], text=True).strip()
    run("xcrun", "swift", "-sdk", sdk, "-target", f"{arch}-apple-macosx13.0", "-module-cache-path", str(stage / "swift-cache"), "distribution/icon.swift", str(png), "terminal" if CONFIG["executable"] == "warp-oss" else "editor")
    for size in (16, 32, 128, 256, 512):
        for scale in (1, 2):
            name = f"icon_{size}x{size}" + ("@2x" if scale == 2 else "") + ".png"
            run("sips", "-z", str(size * scale), str(size * scale), str(png), "--out", str(iconset / name))
    run("iconutil", "-c", "icns", str(iconset), "-o", str(resources / "Personal.icns"))
    if CONFIG["executable"] == "zed":
        shutil.copy2(resources / "Personal.icns", resources / "Document.icns")
    sha = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    notice = resources / "Personal-Source-and-Licenses"
    notice.mkdir(exist_ok=True)
    for license_file in ROOT.glob("LICENSE*"):
        if license_file.is_file():
            shutil.copy2(license_file, notice)
    for license_file in (ROOT / "assets/licenses.md", ROOT / "distribution/README.md"):
        if license_file.is_file():
            shutil.copy2(license_file, notice / license_file.name)
    provenance = {**CONFIG, "version": version, "commit": sha,
                  "source_url": f'https://github.com/{CONFIG["repository"]}/tree/{sha}',
                  "signing": "ad-hoc; not notarized", "architecture": arch}
    (notice / "build.json").write_text(json.dumps(provenance, indent=2) + "\n")
    run("bash", "distribution/sign-macos.sh", str(target), "adhoc")
    with plist_path.open("rb") as stream:
        actual = plistlib.load(stream)
    assert actual["CFBundleIdentifier"] == CONFIG["bundle_id"]
    assert actual["CFBundleExecutable"] == CONFIG["executable"]
    run("lipo", str(target / "Contents/MacOS" / CONFIG["executable"]), "-verify_arch", arch)
    output = ROOT / "dist"
    output.mkdir(exist_ok=True)
    asset = f'{CONFIG["cask"]}-{version}-macos-{arch}.zip'
    archive = output / asset
    archive.unlink(missing_ok=True)
    run("ditto", "-c", "-k", "--sequesterRsrc", "--keepParent", str(target), str(archive))
    with archive.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    (output / f"manifest-{arch}.json").write_text(json.dumps({**provenance, "asset": asset, "sha256": digest}, indent=2) + "\n")

if __name__ == "__main__":
    package(*sys.argv[1:])
