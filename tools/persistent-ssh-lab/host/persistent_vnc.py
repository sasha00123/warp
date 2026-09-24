#!/usr/bin/env python3
"""Keep one private Tart VNC connection alive for isolated GUI acceptance.

Commands arrive as JSON lines on stdin. This never connects to host screen
sharing: the endpoint must be Tart's loopback TCP endpoint supplied in the
environment, and captures are restricted to the lab's output directory.
"""
import json
import os
from pathlib import Path
import sys
from urllib.parse import unquote, urlsplit

from vncdotool import api
from vncdotool.client import KEYMAP
from twisted.internet import reactor


def main():
    endpoint = urlsplit(os.environ["ETERNALWARP_VNC_URL"])
    if endpoint.scheme != "vnc" or endpoint.hostname != "127.0.0.1" or not endpoint.port:
        raise ValueError("Expected the private Tart loopback endpoint")
    output_root = Path(os.environ["ETERNALWARP_OUTPUT_ROOT"]).resolve(strict=True)
    client = api.connect(
        f"127.0.0.1::{endpoint.port}",
        password=unquote(endpoint.password or ""),
        username=unquote(endpoint.username or "") or None,
        timeout=30,
    )
    print(json.dumps({"connected": True}), flush=True)
    for line in sys.stdin:
        try:
            command = json.loads(line)
            action = command["action"]
            if action == "capture":
                destination = (output_root / command["name"]).resolve()
                destination.relative_to(output_root)
                if destination.suffix != ".png":
                    raise ValueError("Captures must be PNG files")
                client.captureScreen(str(destination))
                result = {"capture": str(destination)}
            elif action == "click":
                x, y = int(command["x"]), int(command["y"])
                if not (0 <= x < 8192 and 0 <= y < 8192):
                    raise ValueError("Invalid VM display coordinate")
                client.mouseMove(x, y)
                client.mousePress(int(command.get("button", 1)))
                result = {"clicked": [x, y]}
            elif action == "key":
                key = command["key"]
                key = "bsp" if key == "backspace" else key
                if any(len(part) != 1 and part not in KEYMAP for part in key.split("-")):
                    raise ValueError("Unsupported VNC key name")
                client.keyPress(key)
                result = {"key_sent": True}
            else:
                raise ValueError("Supported actions: capture, click, key")
            print(json.dumps({"ok": True, **result}), flush=True)
        except Exception as error:
            # vncdotool chains calls on one Deferred. A bad key can otherwise
            # poison every later capture/click on an otherwise healthy socket.
            if client.protocol is not None:
                reactor.callFromThread(client.factory.deferred.addErrback, lambda _: client.protocol)
            print(json.dumps({"ok": False, "error": str(error)}), flush=True)
    # Deliberately keep the connection until stdin closes. Repeated connections
    # can crash Apple's experimental VNC accessor on the current host OS.
    client.disconnect()
    api.shutdown()


if __name__ == "__main__":
    main()
