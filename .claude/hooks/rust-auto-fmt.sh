#!/bin/bash

set -uo pipefail

INPUT=$(cat)
FILE=$(echo "$INPUT" | jq -r '.tool_input.file_path // empty')
CWD=$(echo "$INPUT" | jq -r '.cwd // "."')

# Only process .rs files
if ! echo "$FILE" | grep -qE '\.rs$'; then
  exit 0
fi

cd "$CWD"

# Auto-format the edited file
if command -v rustfmt >/dev/null 2>&1; then
  rustfmt "$FILE" 2>/dev/null || true
fi

# Quick clippy feedback (stderr goes back to Claude as context)
CLIPPY_OUT=$(cargo clippy --message-format=short 2>&1 | grep -E '^(error|warning)\[' | head -8 || true)
if [ -n "$CLIPPY_OUT" ]; then
  echo "📎 Clippy findings after editing $(basename "$FILE"):" >&2
  echo "$CLIPPY_OUT" >&2
fi

exit 0