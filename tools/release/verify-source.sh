#!/usr/bin/env bash
# Run only from the trusted workflow checkout, before executing candidate code.
set -euo pipefail

if [ "$#" -ne 3 ]; then
  echo 'usage: verify-source.sh TAG COMMIT TAG_OBJECT' >&2
  exit 1
fi
if [ "${GITHUB_REF:-}" != refs/heads/main ]; then
  echo 'release source verification must run from main' >&2
  exit 1
fi
release_tag="$1"
requested_commit="$2"
expected_tag_object="$3"
[[ "$release_tag" =~ ^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]
[[ "$requested_commit" =~ ^[a-f0-9]{40}$ ]]
[[ "$expected_tag_object" =~ ^[a-f0-9]{40}$ ]]

git fetch --no-tags origin refs/heads/main:refs/remotes/origin/main
git fetch --no-tags origin "refs/tags/$release_tag:refs/tags/$release_tag"
test "$(git cat-file -t "refs/tags/$release_tag")" = tag
actual_tag_object="$(git rev-parse "refs/tags/$release_tag")"
test "$actual_tag_object" = "$expected_tag_object" || {
  echo 'release tag object changed' >&2
  exit 1
}
test "$(git cat-file -p "$actual_tag_object" | sed -n '2p')" = 'type commit'
verified_commit="$(git rev-parse "refs/tags/$release_tag^{commit}")"
test "$verified_commit" = "$requested_commit" || {
  echo 'release tag does not select the requested commit' >&2
  exit 1
}
git merge-base --is-ancestor "$verified_commit" refs/remotes/origin/main || {
  echo 'release source must belong to main' >&2
  exit 1
}
printf '%s\n' "$verified_commit"
