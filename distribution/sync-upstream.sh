#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
: "${GH_REPO:?}"
read -r upstream branch < <(python3 -c 'import json; c=json.load(open("distribution/config.json")); print(c["upstream"], c["upstream_branch"])')
if [[ "$GH_REPO" != "sasha00123/${upstream##*/}" ]]; then
  echo "Unexpected fork repository" >&2
  exit 1
fi
git fetch --no-tags origin main personal/main
git fetch --no-tags "https://github.com/$upstream.git" "$branch"
sha=$(git rev-parse FETCH_HEAD)
# Never rewrite main, including when upstream itself rewrites history.
git merge-base --is-ancestor origin/main "$sha"
git push origin "$sha:refs/heads/main"
if ! git merge-base --is-ancestor "$sha" origin/personal/main; then
  existing=$(gh pr list --repo "$GH_REPO" --base personal/main --head main --state open --json number --jq 'length')
  if [[ "$existing" == 0 ]]; then
    gh pr create --repo "$GH_REPO" --base personal/main --head main \
      --title "Integrate upstream updates" \
      --body "Advance the personal distribution to upstream $sha. Main was updated by fast-forward only. Resolve conflicts in an integration branch and run Personal CI manually with source_ref=refs/pull/NUMBER/merge before merging: GITHUB_TOKEN-created PRs do not automatically start CI."
  fi
fi
