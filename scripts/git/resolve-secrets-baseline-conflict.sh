#!/usr/bin/env bash
# -----------------------------------------------------------------------------
# Git merge driver for .secrets.baseline
#
# Registered in .gitattributes:
#   .secrets.baseline merge=secrets-baseline
#
# Registered in git config (via `make configure-git`):
#   git config merge.secrets-baseline.driver \
#     "scripts/git/resolve-secrets-baseline-conflict.sh %O %A %B %P"
#
# On conflict the driver discards both sides and regenerates the baseline
# from the working tree, preserving existing audit decisions (is_secret).
# This avoids JSON merge conflicts while keeping human audits intact.
# The driver fails closed: if the regenerated baseline contains findings
# without an is_secret audit decision the merge is rejected.
# -----------------------------------------------------------------------------
set -euo pipefail

ANCESTOR="$1"   # %O — common ancestor version
CURRENT="$2"    # %A — current branch version (written back on success)
OTHER="$3"      # %B — incoming branch version
BASENAME="$4"   # %P — path of the file being merged

DETECT_SECRETS_SPEC="git+https://github.com/ibm/detect-secrets.git@076672a9a01abdfc7ecee2e7d14f08cdccb73976"
EXCLUDE="(?x)(Cargo\.lock$|\.lock$|target/|^\.secrets\.baseline$)"

echo "🔀 secrets-baseline merge driver: regenerating $BASENAME from working tree..."

# Merge existing audit decisions from both sides into a temp file so we
# can propagate is_secret=false/true for already-reviewed findings.
MERGED_AUDITS=$(mktemp)
trap 'rm -f "$MERGED_AUDITS"' EXIT

# Combine is_secret decisions from ancestor + incoming into a lookup.
# Strategy: incoming wins over ancestor; current (working) wins over both.
python3 - "$ANCESTOR" "$CURRENT" "$OTHER" "$MERGED_AUDITS" <<'PYEOF'
import json, sys

def load(path):
    try:
        with open(path) as f:
            return json.load(f)
    except Exception:
        return {"results": {}}

ancestor  = load(sys.argv[1])
current   = load(sys.argv[2])
other     = load(sys.argv[3])
out_path  = sys.argv[4]

# Build hash → is_secret map, later sources win
audits = {}
for baseline in (ancestor, other, current):
    for _file, findings in baseline.get("results", {}).items():
        for f in findings:
            h = f.get("hashed_secret")
            if h and "is_secret" in f:
                audits[h] = f["is_secret"]

with open(out_path, "w") as f:
    json.dump(audits, f)
PYEOF

# Regenerate the baseline from the current working tree.
if command -v uv >/dev/null 2>&1; then
    uv tool run --from "$DETECT_SECRETS_SPEC" detect-secrets scan \
        --use-all-plugins \
        --exclude-files "$EXCLUDE" \
        > "$CURRENT.new"
elif command -v detect-secrets >/dev/null 2>&1; then
    detect-secrets scan \
        --use-all-plugins \
        --exclude-files "$EXCLUDE" \
        > "$CURRENT.new"
else
    echo "❌ detect-secrets not found; install via: uv tool install git+https://github.com/ibm/detect-secrets.git@076672a9a01abdfc7ecee2e7d14f08cdccb73976" >&2
    exit 1
fi

# Re-apply audit decisions from the merged audits map.
python3 - "$CURRENT.new" "$MERGED_AUDITS" "$CURRENT" <<'PYEOF'
import json, sys

with open(sys.argv[1]) as f:
    baseline = json.load(f)
with open(sys.argv[2]) as f:
    audits = json.load(f)

for _file, findings in baseline.get("results", {}).items():
    for finding in findings:
        h = finding.get("hashed_secret")
        if h and h in audits:
            finding["is_secret"] = audits[h]

with open(sys.argv[3], "w") as f:
    json.dump(baseline, f, indent=2)
    f.write("\n")
PYEOF

rm -f "$CURRENT.new"

# Fail closed: reject unaudited findings introduced by the merge.
UNAUDITED=$(python3 - "$CURRENT" <<'PYEOF'
import json, sys
with open(sys.argv[1]) as f:
    baseline = json.load(f)
count = sum(
    1
    for findings in baseline.get("results", {}).values()
    for f in findings
    if "is_secret" not in f
)
print(count)
PYEOF
)

if [ "$UNAUDITED" -gt 0 ]; then
    echo "❌ $BASENAME has $UNAUDITED unaudited finding(s) after merge. Audit them with:" >&2
    echo "   detect-secrets audit $BASENAME" >&2
    exit 1
fi

echo "✅ $BASENAME regenerated and audit decisions preserved."
exit 0
