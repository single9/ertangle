#!/bin/sh
# 從原始碼抽出 `tr!` 字串產生 locales/termade.pot，再用 msgmerge 更新每個 locales/*.po。
# 需要 xtr (`cargo install xtr`，只當開發工具，不加進 Cargo.toml) 與 GNU gettext 的 msgmerge。
set -eu
cd "$(dirname "$0")/.."

pot=locales/termade.pot
# 只記檔名不記行號，避免改一行程式碼就讓 .pot 整份變動。
xtr --package-name termade -k tr -k tr_noop -n file -o "$pot.tmp" \
    src/main.rs crates/*/src/lib.rs
# POT-Creation-Date 每次都不同，拿掉才能比對 .pot 是否過期。
grep -v '^"POT-Creation-Date:' "$pot.tmp" > "$pot"
rm -f "$pot.tmp"

for po in locales/*.po; do
    msgmerge --quiet --update --backup=none --no-wrap --add-location=file "$po" "$pot"
done
