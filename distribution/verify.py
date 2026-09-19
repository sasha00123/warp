#!/usr/bin/env python3
import json
import re
import subprocess
from pathlib import Path

root = Path(__file__).resolve().parent.parent
config = json.loads((root / "distribution/config.json").read_text())
assert re.fullmatch(r"io\.sasha00123\.[A-Za-z]+", config["bundle_id"])
assert config["app_name"] not in ("Zed", "Zed Dev", "Warp", "WarpOss")
for path in (root / "distribution").glob("*.sh"):
    subprocess.run(["bash", "-n", str(path)], check=True)
for path in (root / ".github/workflows").glob("personal-*.yml"):
    text = path.read_text()
    assert "pull_request_target" not in text
    assert "secrets." not in text, "Unsigned workflows must not receive signing credentials"
    assert not re.search(r"^\s+runs-on:.*self-hosted", text, re.M)
if config["executable"] == "zed":
    build = (root / "distribution/build-macos.sh").read_text()
    assert "export ZED_UPDATE_EXPLANATION=" in build
    assert 'pub const APP_NAME: &str = "SashaEdit";' in (root / "crates/paths/src/paths.rs").read_text()
    source = (root / "crates/auto_update/src/auto_update.rs").read_text()
    poll = source.split("pub fn poll(", 1)[1].split("if check_type.is_manual()", 1)[0]
    assert 'option_env!("ZED_UPDATE_EXPLANATION").is_some()' in poll and "return;" in poll
else:
    source = (root / "app/src/bin/oss.rs").read_text()
    assert 'AppId::new("io", "sasha00123", "SashaTerm")' in source
    assert "autoupdate_config: None" in source and "Channel::Oss" in source
    assert '--channel oss' in (root / "distribution/build-macos.sh").read_text()
    updater = (root / "app/src/autoupdate/mod.rs").read_text()
    assert "Channel::Oss" in updater and "don't support autoupdate" in updater
print("Distribution identity, updater policy and unsigned workflow boundaries verified")
