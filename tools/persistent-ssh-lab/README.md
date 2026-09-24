# Persistent SSH validation lab

This contains the baseline fixtures and production persistent-workspace test harness.
The native transport, remote journal, picker, and recovery UI are implemented in the
application. Passing baseline or backend tests alone does not authorize a release.
See [persistent workspace behavior](../../docs/persistent-workspaces.md) for the
current candidate's retention and recovery contract.

## Isolation

- GUI testing runs only inside a macOS VM. Never start, quit, click, type into,
  or change permissions for the host's Warp application.
- The VM launcher disables its window, audio, and clipboard sharing. It shares
  only dedicated artifact and guest build-script directories, read-only; not the host home directory,
  SSH agent, credentials, or checkout.
- Tart auto-pruning is disabled. VM setup requires a conservative 100 GiB free
  storage budget; it never deletes existing VM images or build caches.
- Docker tests use a disposable Linux SSH server, test-only keys, pinned host
  keys, a random loopback-only port, and unique tmux sockets. No real remote
  servers or personal tmux sessions are touched.
- The container limits CPU to one core and RAM to 256 MiB. Build and VM workloads
  still consume host resources; headless operation prevents desktop interaction,
  not all performance impact.

## Linux SSH/tmux baseline

From this directory:

```sh
bash ssh-fixture.sh up
bash ssh-fixture.sh test
bash ssh-fixture.sh down
```

The test result is `.state/baseline-results.json`. The tests establish the
behavior available to a future extension backend: new windows, switching,
abrupt SSH disconnect, same-process reattachment, Ctrl-C, interactive stdin,
scrollback replay, and exact-target window deletion. They do not launch Warp,
validate native blocks, test the SSH extension protocol, or simulate app restart.

The fixture's SSH port intentionally cannot be reached from the macOS guest.
Guest end-to-end testing will need a separately configured guest-reachable test
target on an isolated network. Do not expose this fixture on all host interfaces
as a shortcut.

## macOS GUI lane

Tart 2.37.0 and VM storage now live under
`~/Library/Application Support/EternalWarpLab`. This lets the one-shot background
downloader operate without accessing macOS-protected Documents. Set `TART` for
a different installation, or `VM_RUNTIME` for a different runtime directory.
Use an APFS volume for `TART_HOME`.

The download is supervised by the explicitly non-retrying user launchd job
`org.eternalwarp.vm-download-once`. Its plist is not in LaunchAgents, so it is not
a login item. Progress, errors, and final exit status are respectively in
`logs/download.log`, `logs/download.err`, and `logs/download.exit` under the
runtime directory. An absent exit-status file means completion is unknown; also
check the job's PID and log progress. The former workspace `.vm` is no longer
the active VM store.

```sh
TART_HOME=/Volumes/TestSSD/eternalwarp-vms bash vm.sh prepare
TART_HOME=/Volumes/TestSSD/eternalwarp-vms bash vm.sh run
# Separate terminal: commands execute in the guest, not on the host.
TART_HOME=/Volumes/TestSSD/eternalwarp-vms bash vm.sh exec /usr/bin/sw_vers
TART_HOME=/Volumes/TestSSD/eternalwarp-vms bash vm.sh stop
```

Default guest image: `ghcr.io/cirruslabs/macos-tahoe-base:latest`, with the Tart
guest agent. For release validation, pin `VM_IMAGE` to a recorded OCI digest
rather than a moving tag. Do not assume host-key, Accessibility, or Screen
Recording approvals inside the guest are already configured. Any approvals
must be granted inside the guest only.

Put candidate app bundles in `.state/artifacts`, or set `ARTIFACTS_DIR` to a
dedicated directory. They appear at `/Volumes/My Shared Files/artifacts` in the
guest. Build artifacts, guest screenshots, and logs are not automatically
published.

### Guest-only Warp and Zed build caches

The VM is configured with two CPUs and 12 GiB RAM. Copy or clone sources into
`~/Developer` inside the guest, and use the build entry point rather than running
Cargo in a host checkout:

```sh
bash vm.sh build warp /Users/admin/Developer/warp cargo build --release -p warp
bash vm.sh build zed /Users/admin/Developer/zed cargo build --release -p zed
```

These are generic compilation examples, not the custom release packaging commands.
Guest toolchains, native dependencies, source transfer, and custom packaging must
still be prepared before a release build. The helper refuses execution on a
physical Mac and rejects source directories outside the guest's `~/Developer`.
Do not pass explicit host/shared `--target-dir` paths in build commands.

The guest stores Cargo downloads in `~/Library/Caches/EternalWarpBuild/cargo`,
and separates target output under `targets/warp` and `targets/zed`. It also
sets guest-local sccache/XDG cache locations and caps build parallelism at two.
Host-global Cargo and compiler caches remain untouched because other projects
may depend on them. Normal host Cargo commands can still create host caches;
the VM helper is the designated entry point for this work.

Guest caches consume physical storage through the VM disk. This isolates and
centralizes them; it does not eliminate their storage cost. Do not simultaneously
retain a second build-cache copy in host source checkouts.

## Implementation contract

Use the SSH extension's existing authenticated stdio protocol for structured
management and terminal events. Never type bootstrap, selection, deletion, or
replay commands into the user's foreground shell.

1. Add negotiated persistent-terminal capability and explicit unsupported-server
   handling. Client and custom Linux extension artifacts must be released together.
2. Address managed workspaces using durable identities plus a tmux server
   generation. A raw `%pane` or `@window` number is not a durable identity across
   server restarts. Keep attach and create separate; retrying a create request
   must not duplicate a workspace.
3. Keep tmux processes independent of SSH, proxy, daemon grace-period shutdown,
   tab lifetime, and application lifetime. Closing a tab detaches; the explicit
   terminate action kills its selected workspace only.
4. Use the durable output journal and separate authenticated protocol I/O for live
   bytes, resize, stdin, and interrupt. Restore the original process, not a new shell
   or rerun command.
5. Persist shell-integration events and raw terminal output remotely while the
   client is offline. Replay with sequence cursors, deduplication, an atomic
   replay/live handoff, and explicit retention/gap reporting. Scrollback alone
   cannot recreate native Warp block boundaries or command status reliably.
6. Integrate the backend as a terminal transport, not an output-only viewer.
   Native command editing, terminal input, full-screen applications, and block
   state must all use the same selected remote pane.
7. Store the selected workspace locally per tab and expose a native picker after
   warpification. Report disconnected/unknown separately from running/idle;
   do not infer a command's completion from a quiet output stream.

## Release gate: every item is required

| Gate | Required evidence |
| --- | --- |
| Build | Candidate macOS bundle and matching Linux extension builds, source revision and SHA-256 hashes |
| Targeted tests | Protocol, backend, persistence, replay, reconnect, and picker tests plus relevant lint/format checks |
| Guest connection | Fresh SSH connection automatically warpifies; no bootstrap/control text in blocks |
| Editing and interaction | Native multiline editing; stdin and Ctrl-C reach the running job; resize and full-screen app behavior |
| Switching | Two active jobs; switch repeatedly without restarting, misrouting input, or mixing output |
| Reconnect | Abrupt network loss and full app quit/reopen return to the same job, with output produced while offline |
| History and state | Replay retains block boundaries, exit status, colors, and ordering without duplicate commands/output |
| Lifecycle | Close/reopen tab preserves jobs; explicit termination affects only the intended workspace; stale IDs fail safely |
| Compatibility | Existing ordinary SSH still works; missing tmux and incompatible extensions produce actionable errors |
| Publication | Only the exact tested artifacts are uploaded; fork release and Homebrew cask updated together |

Nothing in these scripts publishes, tags a release, changes the installed app,
or marks unexecuted guest checks as passed.

## Production backend implementation tests

The harness compiles structured create/list/resolve/terminate and terminal RPCs,
the Unix tmux backend, journal retention/compression, shell startup, and replay cursor
logic. The candidate advertises native transport and block replay; the app rejects
older management-only extensions. Native model, editor, picker, and reconnect tests
also run in the macOS application suite, followed by actual GUI acceptance.

`rpc-harness` compiles the production backend and protobuf definitions independently
of the GUI dependency graph. On an isolated Unix guest with Rust 1.92, protobuf, and
tmux 3.2 or newer:

```sh
cd ~/Developer/warp
bash tools/persistent-ssh-lab/guest/build.sh warp "$PWD" \
  cargo test --locked --manifest-path tools/persistent-ssh-lab/rpc-harness/Cargo.toml \
  -- --include-ignored --test-threads=1
```

The build helper above is macOS-VM-only. Linux backend tests can compile
`crates/remote_server/src/persistent_workspace.rs` directly with `rustc --test`
and run with `--include-ignored --test-threads=1`. The tests use their own tmux
namespaces and must not be substituted for native app acceptance tests.
