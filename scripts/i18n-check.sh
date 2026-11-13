#!/bin/sh
# CI 檢查翻譯檔：.pot 與原始碼同步、各 .po 沒有 fuzzy 與缺譯、佔位符一致。
set -eu
cd "$(dirname "$0")/.."

fail=0
pot=locales/termade.pot
cp "$pot" "$pot.orig"
for po in locales/*.po; do cp "$po" "$po.orig"; done
sh scripts/i18n-extract.sh
if ! cmp -s "$pot" "$pot.orig"; then
    echo "locales/termade.pot 過期：請執行 make i18n-extract 並提交結果。" >&2
    diff -u "$pot.orig" "$pot" >&2 || true
    fail=1
fi
for po in locales/*.po; do
    if ! cmp -s "$po" "$po.orig"; then
        echo "$po 與 termade.pot 不同步：請執行 make i18n-extract 並提交結果。" >&2
        diff -u "$po.orig" "$po" >&2 || true
        fail=1
    fi
    mv "$po.orig" "$po"
done
mv "$pot.orig" "$pot"

for po in locales/*.po; do
    python3 scripts/i18n-lint.py "$po" || fail=1
done
exit $fail
