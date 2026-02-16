#!/bin/bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

echo "=== Step 1: Deterministic checks ==="
./tools/verify_dd_v2.sh || true  # Report but don't block Codex review

echo ""
echo "=== Step 2: Codex CLI semantic review ==="
# codex exec runs non-interactively with file read access
codex exec -c 'sandbox_permissions=["disk-full-read-access"]' \
  "$(cat tools/codex_review_dd3.md)"
