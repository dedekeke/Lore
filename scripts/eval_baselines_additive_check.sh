#!/usr/bin/env bash
# Guard against drift smuggled in via "fixture-add regen" PRs.
#
# eval/README.md draws a line between two regen kinds:
#   - drift regen   : code change moves scores → dedicated PR, review-gated
#   - fixture-add   : new cases only, pre-existing rows byte-identical → OK
#
# This script enforces the second contract. Given the base-ref baselines.json
# and the PR's baselines.json, it asserts:
#   1. `version` and `metadata` are byte-identical (canonical JSON).
#   2. Every dataset present in the base is still present.
#   3. Every `case_id` present in the base's per_case[] arrays is still
#      present in the PR's, with byte-identical canonical JSON.
# New `case_id` rows in the PR are allowed.
#
# Usage: eval_baselines_additive_check.sh OLD_FILE NEW_FILE

set -euo pipefail

OLD="${1:?usage: $0 OLD_FILE NEW_FILE}"
NEW="${2:?usage: $0 OLD_FILE NEW_FILE}"

if ! command -v jq >/dev/null 2>&1; then
  echo "::error::jq not found in PATH" >&2
  exit 2
fi

drift=0

# Pre-flight: case_id values must be unique within each dataset.per_case[].
# If duplicates exist, `select(.case_id == $cid)` would emit multiple rows and
# silently mask drift. Fail fast with a clear error rather than guessing.
for f in "$OLD" "$NEW"; do
  if ! jq -e '
    [.datasets[]?.per_case[]?.case_id] as $ids |
    ($ids | length) == ($ids | unique | length)
  ' "$f" >/dev/null; then
    echo "::error::$f has duplicate case_id values within a dataset.per_case[]; refusing to compare"
    exit 2
  fi
done

old_version=$(jq -cS '.version' "$OLD")
new_version=$(jq -cS '.version' "$NEW")
if [[ "$old_version" != "$new_version" ]]; then
  echo "::error::version drift: $old_version -> $new_version (drift regen, not fixture-add)"
  drift=1
fi

old_meta=$(jq -cS '.metadata' "$OLD")
new_meta=$(jq -cS '.metadata' "$NEW")
if [[ "$old_meta" != "$new_meta" ]]; then
  echo "::error::metadata drift"
  echo "  old: $old_meta"
  echo "  new: $new_meta"
  drift=1
fi

while IFS= read -r ds; do
  if ! jq -e --arg ds "$ds" '.datasets[$ds]' "$NEW" >/dev/null; then
    echo "::error::dataset '$ds' present in old baseline but missing from new"
    drift=1
    continue
  fi
  while IFS= read -r case_id; do
    if ! old_row=$(jq -cS --arg ds "$ds" --arg cid "$case_id" \
      '[.datasets[$ds].per_case[] | select(.case_id == $cid)] | first // empty' "$OLD"); then
      echo "::error::jq failure reading old row for dataset '$ds' case '$case_id'"
      exit 2
    fi
    if ! new_row=$(jq -cS --arg ds "$ds" --arg cid "$case_id" \
      '[.datasets[$ds].per_case[] | select(.case_id == $cid)] | first // empty' "$NEW"); then
      echo "::error::jq failure reading new row for dataset '$ds' case '$case_id'"
      exit 2
    fi
    if [[ -z "$new_row" ]]; then
      echo "::error::dataset '$ds' case '$case_id' removed (drift regen, not fixture-add)"
      drift=1
    elif [[ "$old_row" != "$new_row" ]]; then
      echo "::error::dataset '$ds' case '$case_id' drifted"
      echo "  old: $old_row"
      echo "  new: $new_row"
      drift=1
    fi
  done < <(jq -r --arg ds "$ds" '.datasets[$ds].per_case[].case_id' "$OLD")
done < <(jq -r '.datasets | keys[]' "$OLD")

if [[ "$drift" -eq 1 ]]; then
  echo
  echo "Pre-existing baseline rows changed. If this is intentional (an embedding"
  echo "or scoring code change), submit it as a *drift regen* in a dedicated PR"
  echo "per eval/README.md. Otherwise, recompute eval/baselines.json with the"
  echo "additive-only path so existing rows stay byte-identical."
  exit 1
fi

echo "ok: pre-existing baseline rows are byte-identical; new rows allowed."
