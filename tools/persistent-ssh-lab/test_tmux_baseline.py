#!/usr/bin/env python3
"""Real SSH/tmux baseline tests. These do NOT validate the Warp application."""

import argparse
import json
import os
import re
import select
import shlex
import subprocess
import time
import unittest
import uuid
from datetime import datetime, timezone
from pathlib import Path


class ControlClient:
    def __init__(self, ssh, socket):
        command = shlex.join(["tmux", "-L", socket, "-C", "attach-session", "-t", "jobs"])
        self.process = subprocess.Popen(
            [*ssh, command], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        self.buffer = b""
        deadline = time.monotonic() + 10
        try:
            while not self.line(deadline).startswith(b"%session-changed "):
                pass
        except BaseException:
            self.disconnect()
            raise

    def line(self, deadline):
        while b"\n" not in self.buffer:
            remaining = deadline - time.monotonic()
            if remaining <= 0 or not select.select([self.process.stdout], [], [], remaining)[0]:
                raise TimeoutError("No response from tmux control mode")
            chunk = os.read(self.process.stdout.fileno(), 65536)
            if not chunk:
                raise EOFError("tmux control connection closed")
            self.buffer += chunk
        line, self.buffer = self.buffer.split(b"\n", 1)
        return line.rstrip(b"\r")

    def request(self, *args):
        self.process.stdin.write((shlex.join(args) + "\n").encode())
        self.process.stdin.flush()
        deadline = time.monotonic() + 10
        started = False
        output = []
        while True:
            line = self.line(deadline)
            if line.startswith(b"%begin "):
                started = True
            elif started and line.startswith(b"%end "):
                return b"\n".join(output)
            elif started and line.startswith(b"%error "):
                raise RuntimeError(b"\n".join(output).decode(errors="replace"))
            elif started:
                output.append(line)

    def input(self, pane, data):
        self.request("send-keys", "-t", pane, "-H", *[f"{byte:02x}" for byte in data])

    def disconnect(self):
        if self.process.poll() is None:
            self.process.kill()
        self.process.communicate(timeout=10)


class TmuxBaselineTests(unittest.TestCase):
    ssh = None

    def setUp(self):
        self.socket = "ew-test-" + uuid.uuid4().hex
        self.clients = []
        self.addCleanup(self.cleanup)
        result = self.tmux(
            "new-session", "-d", "-s", "jobs", "-n", "first", "-P", "-F",
            "#{window_id} #{pane_id} #{pane_pid}",
        )
        self.window, self.pane, self.pid = result.stdout.strip().split()
        self.tmux("set-option", "-g", "history-limit", "5000")

    def cleanup(self):
        for client in self.clients:
            client.disconnect()
        self.tmux("kill-server", check=False)

    def tmux(self, *args, check=True):
        command = shlex.join(["tmux", "-L", self.socket, "-f", "/dev/null", *args])
        return subprocess.run(
            [*self.ssh, command], capture_output=True, text=True,
            timeout=15, check=check,
        )

    def connect(self):
        client = ControlClient(self.ssh, self.socket)
        self.clients.append(client)
        return client

    def history(self, pane=None):
        return self.tmux("capture-pane", "-p", "-S", "-", "-t", pane or self.pane).stdout

    def eventually(self, predicate, description):
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            if predicate():
                return
            time.sleep(0.1)
        self.fail(description)

    def ticks(self):
        return [int(value) for value in re.findall(r"^EW_TICK:(\d+)$", self.history(), re.M)]

    def start_ticks(self, client):
        client.input(
            self.pane,
            b"i=0; while :; do printf 'EW_TICK:%s\\n' \"$i\"; i=$((i+1)); sleep 0.2; done\r",
        )
        self.eventually(lambda: len(self.ticks()) >= 3, "Job did not start")

    def test_new_window_does_not_reuse_existing_pane(self):
        result = self.tmux(
            "new-window", "-d", "-t", "jobs", "-n", "second", "-P", "-F",
            "#{window_id} #{pane_id}",
        )
        window, pane = result.stdout.strip().split()
        self.assertNotEqual(self.window, window)
        self.assertNotEqual(self.pane, pane)
        self.assertIn(self.pane, self.tmux("list-panes", "-a", "-F", "#{pane_id}").stdout)

    def test_abrupt_disconnect_keeps_job_and_reconnects_same_pane(self):
        client = self.connect()
        self.start_ticks(client)
        before = max(self.ticks())
        client.disconnect()
        self.eventually(lambda: max(self.ticks()) > before + 2, "Job stopped with SSH client")
        resumed = self.connect()
        identity = resumed.request("display-message", "-p", "-t", self.pane, "#{pane_id} #{pane_pid}")
        self.assertEqual(identity.decode(), f"{self.pane} {self.pid}")

    def test_switch_windows_while_original_job_continues(self):
        client = self.connect()
        self.start_ticks(client)
        before = max(self.ticks())
        second = self.tmux("new-window", "-d", "-t", "jobs", "-P", "-F", "#{window_id}").stdout.strip()
        client.request("select-window", "-t", second)
        self.eventually(lambda: max(self.ticks()) > before + 2, "Switching stopped the job")
        client.request("select-window", "-t", self.window)
        self.assertEqual(client.request("display-message", "-p", "#{pane_id}").decode(), self.pane)

    def test_ctrl_c_reaches_foreground_job_after_reconnect(self):
        client = self.connect()
        self.start_ticks(client)
        client.disconnect()
        resumed = self.connect()
        resumed.input(self.pane, b"\x03")
        resumed.input(self.pane, b"printf 'EW_AFTER_INTERRUPT\\n'\r")
        self.eventually(
            lambda: re.search(r"^EW_AFTER_INTERRUPT$", self.history(), re.M),
            "Ctrl-C failed to interrupt the foreground job",
        )
        count = len(self.ticks())
        time.sleep(0.6)
        self.assertEqual(len(self.ticks()), count)

    def test_stdin_reaches_existing_read_after_reconnect(self):
        client = self.connect()
        client.input(self.pane, b"read -r answer; printf 'EW_REPLY:%s\\n' \"$answer\"\r")
        client.disconnect()
        resumed = self.connect()
        resumed.input(self.pane, b"hello from resumed client\r")
        self.eventually(
            lambda: re.search(r"^EW_REPLY:hello from resumed client$", self.history(), re.M),
            "Input did not reach the original foreground read",
        )

    def test_history_survives_disconnect_beyond_visible_screen(self):
        client = self.connect()
        client.input(self.pane, b"for i in $(seq 1 1000); do printf 'EW_HISTORY:%s\\n' \"$i\"; done\r")
        self.eventually(lambda: "\nEW_HISTORY:1000\n" in self.history(), "Output did not complete")
        client.disconnect()
        self.connect()
        history = self.history()
        self.assertRegex(history, r"(?m)^EW_HISTORY:1$")
        self.assertRegex(history, r"(?m)^EW_HISTORY:1000$")

    def test_close_window_does_not_kill_other_job(self):
        client = self.connect()
        self.start_ticks(client)
        before = max(self.ticks())
        second = self.tmux("new-window", "-d", "-t", "jobs", "-P", "-F", "#{window_id}").stdout.strip()
        client.request("kill-window", "-t", second)
        windows = self.tmux("list-windows", "-t", "jobs", "-F", "#{window_id}").stdout.splitlines()
        self.assertEqual(windows, [self.window])
        self.eventually(lambda: max(self.ticks()) > before + 2, "Closing another window killed the job")

    def test_stale_window_id_does_not_delete_current_window(self):
        second = self.tmux("new-window", "-d", "-t", "jobs", "-P", "-F", "#{window_id}").stdout.strip()
        self.tmux("kill-window", "-t", second)
        self.assertNotEqual(self.tmux("kill-window", "-t", second, check=False).returncode, 0)
        windows = self.tmux("list-windows", "-t", "jobs", "-F", "#{window_id}").stdout.splitlines()
        self.assertEqual(windows, [self.window])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", required=True, type=int)
    parser.add_argument("--identity", required=True)
    parser.add_argument("--known-hosts", required=True)
    parser.add_argument("--report", required=True)
    args = parser.parse_args()
    TmuxBaselineTests.ssh = [
        "ssh", "-T", "-F", "/dev/null", "-i", args.identity, "-p", str(args.port),
        "-o", "BatchMode=yes", "-o", "IdentitiesOnly=yes", "-o", "ConnectTimeout=5",
        "-o", "StrictHostKeyChecking=yes", "-o", f"UserKnownHostsFile={args.known_hosts}",
        "tester@127.0.0.1",
    ]
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(TmuxBaselineTests)
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    report = {
        "scope": "ssh_tmux_baseline_only_not_warp",
        "timestamp": datetime.now(timezone.utc).isoformat(),
        "tests": result.testsRun,
        "failures": [str(case) for case, _ in result.failures],
        "errors": [str(case) for case, _ in result.errors],
        "passed": result.wasSuccessful(),
        "release_eligible": False,
    }
    Path(args.report).write_text(json.dumps(report, indent=2) + "\n")
    raise SystemExit(0 if result.wasSuccessful() else 1)


if __name__ == "__main__":
    main()

