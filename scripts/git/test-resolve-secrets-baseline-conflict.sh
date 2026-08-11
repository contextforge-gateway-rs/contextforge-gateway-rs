#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
PYTHON=$(command -v python3)
TMP_DIR=$(mktemp -d)
trap 'rm -rf "$TMP_DIR"' EXIT

FAKE_BIN="$TMP_DIR/bin"
mkdir -p "$FAKE_BIN"

cat > "$FAKE_BIN/detect-secrets" <<'FAKE_DETECT_SECRETS'
#!/usr/bin/env bash
set -euo pipefail

exclude=''
while (($#)); do
    if [[ "$1" == --exclude-files ]]; then
        exclude=$2
        shift 2
    else
        shift
    fi
done

python3 - "$exclude" <<'PY'
import json
import re
import sys

exclude = sys.argv[1]
try:
    excludes_baseline = re.search(exclude, ".secrets.baseline") is not None
except re.error as error:
    print(error, file=sys.stderr)
    raise

results = {}
results["known.txt"] = [
    {
        "type": "BasicAuthDetector",
        "filename": "known.txt",
        "hashed_secret": "known-audited-value",  # pragma: allowlist secret
    }
]
if not excludes_baseline:
    results[".secrets.baseline"] = [
        {
            "type": "BasicAuthDetector",
            "filename": ".secrets.baseline",
            "hashed_secret": "baseline-self-value",  # pragma: allowlist secret
        }
    ]

if __import__("os").environ.get("FAKE_SCAN_MODE") == "unaudited":
    results["new-value.txt"] = [
        {
            "type": "BasicAuthDetector",
            "filename": "new-value.txt",
            "hashed_secret": "new-unaudited-value",  # pragma: allowlist secret
        }
    ]

print(json.dumps({"results": results}))
PY
FAKE_DETECT_SECRETS
chmod +x "$FAKE_BIN/detect-secrets"

python3 - "$TMP_DIR" <<'PY'
import json
import pathlib
import sys

tmp_dir = pathlib.Path(sys.argv[1])
baseline = {
    "results": {
        "known.txt": [
            {
                "type": "BasicAuthDetector",
                "filename": "known.txt",
                "hashed_secret": "known-audited-value",  # pragma: allowlist secret
                "is_secret": False,
            }
        ]
    }
}
for name in ("ancestor.json", "current.json", "other.json"):
    (tmp_dir / name).write_text(json.dumps(baseline))
PY

PATH="$FAKE_BIN:$(dirname "$PYTHON"):/usr/bin:/bin" \
    FAKE_SCAN_MODE=clean \
    "$SCRIPT_DIR/resolve-secrets-baseline-conflict.sh" \
    "$TMP_DIR/ancestor.json" "$TMP_DIR/current.json" "$TMP_DIR/other.json" \
    .secrets.baseline

python3 - "$TMP_DIR/current.json" <<'PY'
import json
import sys

with open(sys.argv[1]) as stream:
    baseline = json.load(stream)
assert ".secrets.baseline" not in baseline["results"]
assert baseline["results"]["known.txt"][0]["is_secret"] is False
PY

if output=$(env PATH="$FAKE_BIN:$(dirname "$PYTHON"):/usr/bin:/bin" \
    FAKE_SCAN_MODE=unaudited \
    "$SCRIPT_DIR/resolve-secrets-baseline-conflict.sh" \
    "$TMP_DIR/ancestor.json" "$TMP_DIR/current.json" "$TMP_DIR/other.json" \
    .secrets.baseline 2>&1); then
    printf 'merge driver accepted an unaudited finding\n%s\n' "$output" >&2
    exit 1
fi

case "$output" in
    *"has 1 unaudited finding(s)"*) ;;
    *)
        printf 'merge driver rejected for the wrong reason\n%s\n' "$output" >&2
        exit 1
        ;;
esac

echo "merge-driver regression checks passed"
