#!/bin/sh
set -e

REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
HOOKS_DIR="$REPO_ROOT/.git/hooks"

if [ ! -d "$HOOKS_DIR" ]; then
    echo "❌ Error: .git/hooks directory not found. Are you in a git repository?"
    exit 1
fi

cp "$REPO_ROOT/scripts/pre-commit.sh" "$HOOKS_DIR/pre-commit"
chmod +x "$HOOKS_DIR/pre-commit"

echo "✅ Successfully installed pre-commit hook into $HOOKS_DIR/pre-commit."
echo "   Now 'git commit' will automatically verify formatting, clippy, and i18n catalogs."
