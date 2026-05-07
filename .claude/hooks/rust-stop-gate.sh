#!/bin/bash

set -euo pipefail

INPUT=$(cat)
CWD=$(echo "$INPUT" | jq -r '.cwd // "."')
cd "$CWD"

ERRORS=""

# 1. cargo check — compilation errors are a hard stop
if ! cargo check 2>/dev/null; then
  ERRORS="${ERRORS}❌ cargo check failed\n"
fi

# 2. clippy errors (warnings are OK, errors are not)
CLIPPY_ERRORS=$(cargo clippy --message-format=short 2>&1 | grep -c '^error' || true)
if [ "$CLIPPY_ERRORS" -gt 0 ]; then
  ERRORS="${ERRORS}❌ clippy has $CLIPPY_ERRORS error(s)\n"
fi

# 3. tests compile (--no-run = just compile, don't execute)
if ! cargo test --no-run 2>/dev/null; then
  ERRORS="${ERRORS}❌ tests do not compile\n"
fi

if [ -n "$ERRORS" ]; then
  echo -e "🚫 Cannot finish — Rust checks failed:\n$ERRORS" >&2
  echo "Please fix these issues before completing the task." >&2
  exit 2
fi

echo "✅ All Rust checks passed" >&2
exit 0