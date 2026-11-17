#!/bin/sh
set -e

echo "==> Running pre-commit checks..."

# 1. Formatting check
echo "--> Checking code formatting (cargo fmt --check)..."
if ! cargo fmt --check; then
    echo "❌ Error: Code formatting check failed."
    echo "    Please run 'cargo fmt' to format code before committing."
    exit 1
fi

# 2. Clippy check
echo "--> Checking clippy warnings (cargo clippy --all-targets -- -D warnings)..."
if ! cargo clippy --all-targets -- -D warnings; then
    echo "❌ Error: Clippy check failed with warnings/errors."
    echo "    Please fix the clippy issues above before committing."
    exit 1
fi

# 3. i18n catalog check
echo "--> Checking i18n catalogs (make i18n-check)..."
if ! make i18n-check; then
    echo "❌ Error: i18n check failed."
    echo "    Please run 'make i18n-extract' and fill in any missing translations in locales/en.po."
    exit 1
fi

echo "✅ All pre-commit checks passed!"
