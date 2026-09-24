# Persistent SSH workspaces

This describes the current unreleased candidate. Publication requires the isolated-VM
acceptance matrix in `tools/persistent-ssh-lab/README.md`; source implementation alone
does not establish a tested release.

## Everyday use

Connect through Warp's SSH extension and open the native **Workspaces** picker after
warpification. **New workspace** creates an independent remote shell. Selecting an
existing workspace reattaches to that exact incarnation; it never reruns the command.
Commands, output, the editor, input, and interrupts use the selected workspace.

Closing a tab or quitting Warp detaches locally. The remote shell and its jobs remain
in tmux. Reopening restores the saved workspace and replays recorded output. Network
recovery reconnects automatically. While disconnected or catching up, input is paused;
uncertain input is never automatically resent.

The red **x** asks for confirmation before terminating the selected workspace and
deleting its recorded history. It does not close other jobs. An exited shell remains
available with retained output until explicitly deleted.

## History policy

**Settings > Warpify > SSH > Persistent workspace history** selects the policy for
new workspaces. Existing workspaces retain the policy they were created with.

- **Until workspace deletion** is the default. Older output is compressed on the
  remote host; the most recent 64 MiB stays in hot storage. There is no total history
  size cap. This consumes remote disk space until explicit workspace deletion.
- **Most recent 64 MiB** bounds history by discarding older output. A reconnect whose
  cursor predates the retained range cannot fully reconstruct the original blocks.

The picker reports journal storage size when available. At 1 GiB a visible warning
asks you to delete unused workspaces. The counter includes compressed archives and
committed hot output, not filesystem allocation overhead or unrelated files.
These logs can contain terminal secrets; directories are private (0700), and journal
and startup files are private (0600).

## Recovery when history is missing

Network loss is not history loss: recording continues remotely while the laptop is
offline. A recorder failure or the optional rolling policy can, however, leave a real
gap. Warp pauses native replay rather than fabricate complete blocks.

**Open live recovery terminal** opens a separate local Warp pane and attaches to the
same remote tmux session. This is the live terminal screen, not reconstructed Warp
blocks. It preserves the old recorded view and does not restart the job or send setup
commands into its stdin. The recovery connection uses the saved SSH route, strict
host-key checking, no copied port forwards, and an incarnation guard. It cannot attach
to a replacement workspace with a reused numeric tmux ID. A dropped recovery connection
can be reopened from the original workspace pane.

## Requirements and limits

- The matching custom Mac app and Linux SSH extension are installed as a pair.
  Management-only extensions are rejected for native block replay.
- The implementation requires tmux 3.2 or newer; the test lanes exercise older tmux
  3.4 on Linux and newer tmux 3.7c on macOS. Each release must record its actual results.
- Native shell integration supports Bash, Zsh, and Fish. Arbitrary old tmux panes are
  not bootstrapped while a command may be running.
- SSH uses its existing connection and extension protocol. No extra production
  listening port is required.
- This protects against laptop/network/app loss, not a remote-host reboot, tmux server
  termination, storage failure, or an administrator killing the remote job.
- Archived output is not a complete backup. Native replay is sequential and can take
  time for very large journals. Missing bytes are reported explicitly.
