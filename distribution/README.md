# Warp Custom distribution

Unofficial build of Warp; not affiliated with its upstream developer.

## Branches and feature PRs

- `main` is an exact, fast-forward-only mirror of `warpdotdev/warp/master`.
- `feature/*` contains one independent change. Start from a suitable upstream commit, open a PR to `personal/main`, and merge with a merge commit to preserve feature history.
- `personal/main` contains reviewed features and these distribution files. Set it as the GitHub default branch **after merging this PR** so scheduled and manual workflows are discoverable. Never merge distribution code into `main`.
- Upstream sync advances `main` only if it is an ancestor of upstream; divergence fails without force-pushing. Integration is an explicit `main` → `personal/main` PR. A PR created with `GITHUB_TOKEN` does not start CI automatically without approval; approve its workflow runs or run Personal CI manually on the PR merge ref before merging (see below).
- Protect `personal/main` against deletion and force pushes; require the `personal/ci` commit status (validation and both macOS builds). Leave “Require branches to be up to date” unchecked: feature branches must remain based on the upstream mirror, without other personal features merged back into them. CI builds the PR merge commit and reports `personal/ci` on its head, checking that neither parent changed during the build. With this non-strict branch protection, a later base update does not invalidate a successful head status automatically: rerun CI on the current PR merge ref after every base update and before merging another feature. Protect `main` against deletion/force pushes but permit the sync bot's fast-forward updates. Avoid requiring distribution checks on `main`.

## Upstream sync credentials

`GITHUB_TOKEN` cannot push new or changed workflow files from upstream. Sync therefore checks out with a dedicated SSH deploy key that has write access to this fork only. Its private key is the `UPSTREAM_SYNC_SSH_KEY` secret in the `upstream-sync` environment. Restrict that environment to the **branch** `personal/main` (not tags); PR refs and feature branches must not match. The sync job also checks its branch before entering the environment. No private key belongs in Git, release archives, PR CI or local build artifacts. The checkout action verifies GitHub's SSH host key and removes the credential after the job. The normal GitHub token has only contents-read and PR-write access for opening the integration PR.

Keep `main` and `personal/main` protected from force pushes/deletion, including administrators. Rotate a sync credential by creating a new repository-specific deploy key, replacing the environment secret, verifying a sync run, and deleting the old deploy key in repository settings. Deploy keys do not expire automatically. These keys do not provide access to other repositories or the user's account.

## Build and release

Personal CI builds PR merge commits and every push to `personal/main`, using ordinary hosted macOS Apple Silicon and Intel runners. PR jobs have read-only tokens and no signing or sync secrets. Artifacts expire after 14 days.

Run `Personal CI` with ref `personal/main` and `source_ref=refs/pull/NUMBER/merge` to validate a bot-created upstream PR. Its artifact is a test build only. The result is posted as `personal/ci` on the PR head after verifying the head and base still match the tested merge. The same required check applies to bot-created PRs. For outside-fork PRs, a maintainer must run this manual validation after review because their read-only token cannot post commit statuses. Resolve conflicts on a separate integration branch before testing.

Run `Personal Release` on `personal/main` with a numeric `version` such as `2026.9.19`. A version is immutable: reruns refuse existing tags/releases. Both architectures must build successfully before a draft GitHub Release is created. It contains ZIP apps, source archive at the exact built commit, Cargo.lock, source/rebuild instructions, license notices, checksums and a Homebrew manifest. Review the draft and launch the app before publishing it. Never publish a draft whose build or source bundle is incomplete.

The release source archive includes the exact Git tree and recursively initialized submodules. `dependency-sources/` contains registry crate sources and complete tracked Git dependency workspaces at their locked revisions; its `manifest.json` maps packages to source snapshots. Identical crate names/versions from different origins are kept separately, because Cargo's single-directory vendor export cannot represent this dependency graph. The original Cargo.lock is preserved without substitutions. These are corresponding source snapshots, not an offline Cargo source replacement: Cargo resolution, non-Cargo build downloads and toolchains may still require the network. The distribution scripts read Git metadata: to rebuild a packaged release, clone this fork, check out the exact commit in `homebrew.json`, initialize submodules and follow the build commands below. The source archive also supports direct Cargo builds with the original lockfile. Preserve all corresponding sources for as long as distributing binaries.

After publication the separate `sasha00123/homebrew-tap` workflow discovers releases and opens a cask update PR. It uses its own repository token, so no cross-repository PAT is needed. Once the cask PR is merged:

```sh
brew install --cask sasha00123/tap/warp-custom
brew upgrade --cask warp-custom
```

The first cask appears only after a real published release; no placeholder versions or hashes are shipped. For local builds on a Mac with full Xcode, Rust and Homebrew:

```sh
bash distribution/setup-macos.sh
bash distribution/build-macos.sh 2026.9.19
```

## Identity and updates

- App: `Warp Custom.app`
- Bundle ID: `io.sasha00123.WarpCustom`
- URL scheme: `warp-custom`
- Separate user data: `~/.warp-custom` and `~/Library/Application Support/io.sasha00123.WarpCustom`. The official Apple App Group is not used. No automatic migration or deletion of official or prior OSS app settings.
- Unique geometric icon, generated from source by `icon.swift`.
- Upstream self-updating is disabled at compile time/channel configuration; update with Homebrew or GitHub Releases.
- Cloud accounts and upstream-hosted features still depend on the upstream services and their terms. Custom URI schemes may require provider-specific OAuth registration; cloud login callbacks are not certified by this pipeline.

## Signing later

Current bundles use ad-hoc signatures, **not** a trusted Developer ID signature and **not** Apple notarization. Gatekeeper may block the initial launch. Follow macOS Privacy & Security → Open Anyway for a build you trust; this project never disables Gatekeeper or automatically removes quarantine.

`sign-macos.sh APP` is the optional later stage, after packaging/identity changes and before ZIP/checksums. On a protected release runner import your Developer ID Application certificate into a temporary keychain, set `DEVELOPER_ID`, and store notarytool credentials as `NOTARY_PROFILE`. The script signs nested Mach-O code and bundles, submits to Apple, staples the accepted ticket, and verifies. Use your own team/certificate, never upstream credentials. Add this only to a release job protected by an environment; never expose credentials to PR builds. Enable only after a successful full signed/notarized test release.

## License and branding review (2026-09-19)

Primary application license: AGPL-3.0. Keep all original copyright/license files. Reusable components retain their own licenses (Zed includes Apache-2.0; WarpUI uses MIT). Every binary must remain associated with publicly accessible corresponding source including modifications and build scripts. License and source notices are embedded in the bundle. Attribution generation uses `distribution/about.toml` with private-marked crates included and `--fail`; AGPL dependencies must not be silently omitted.

Sources: [Zed software overview](https://zed.dev/software-overview), [Zed brand](https://zed.dev/brand), [Warp FAQ](https://github.com/warpdotdev/warp/blob/master/FAQ.md), repository LICENSE files. The source licenses do not provide permission to imply upstream endorsement. The application names include “Custom”; releases, package descriptions and app metadata explicitly say “unofficial build”. The IDs, protocols and source-drawn Dock icons distinguish these builds from official apps; upstream names in attribution and feature documentation describe origin. This is not a trademark clearance or a promise of upstream cloud support.

## Existing SSH/tmux feature migration

The original checkout remains untouched. Its committed feature (`ba46fa29`, plus prerequisite `cc08ede7`) is preserved as `feature/persistent-ssh-tmux`. The distribution starts at upstream `7ffccbb8`, the base used by that feature, because current upstream has removed/reorganized terminal components and a trial merge produced conflicts in more than 20 files. `main` still mirrors current `warpdotdev/warp/master`; do not force the integration through.

The 17 uncommitted files from the original checkout are preserved separately as a patch/snapshot. The prior task explicitly stopped before committing due to an echo-suppression edge case. They must not be silently included in a friends' release. Porting the tmux feature to current upstream and finishing that edge case require their own reviewed PRs.
