#!/usr/bin/env bash
# Fetch the tier-2 stress core (lowRISC Ibex) at a pinned ref into build/.
# Not vendored into the repo (keeps it lean); fetched in nightly CI / locally.
#
# Reproducibility: set IBEX_REF to a verified commit SHA before relying on this
# in nightly. It defaults to a branch for bring-up convenience, which is NOT
# reproducible. (The sandbox that authored this could not reach github.com to
# pin a SHA; pinning is the first tier-2 hardening task — see ../../docs/fixtures.md.)
set -euo pipefail

IBEX_REF="${IBEX_REF:-master}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEST="$HERE/build/ibex"

mkdir -p "$HERE/build"
if [ ! -d "$DEST/.git" ]; then
  git clone --filter=blob:none https://github.com/lowRISC/ibex.git "$DEST"
fi
git -C "$DEST" fetch origin "$IBEX_REF"
git -C "$DEST" checkout --detach FETCH_HEAD
echo ">> ibex at $(git -C "$DEST" rev-parse HEAD)"
