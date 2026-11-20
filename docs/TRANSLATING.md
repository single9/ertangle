# 翻譯指南

ertangle 的介面以 gettext `.po` 檔翻譯。原始碼裡的繁體中文就是原文 (`msgid`)，各語言的翻譯放在 `locales/<語言>.po`。翻譯者只需要編輯 `.po`，不必改程式碼。

## 檔案

| 檔案 | 用途 |
| --- | --- |
| `locales/ertangle.pot` | 範本：從原始碼抽出的所有原文，由工具產生，不要手改 |
| `locales/en.po` | 英文翻譯 |
| `crates/ertangle-core/src/i18n.rs` | 語言偵測、切換與載入翻譯檔 |

翻譯檔在編譯時內嵌進執行檔，執行時不讀外部檔案。改完 `.po` 要重新編譯才看得到。

## 編輯既有語言

任何支援 gettext 的工具都可以用：

- [Poedit](https://poedit.net/)：開啟 `locales/en.po`，逐條填寫翻譯後儲存。
- 文字編輯器：直接改 `msgstr "…"`。
- Weblate、Crowdin 這類線上平台：匯入 `ertangle.pot` 與 `.po` 即可。

改完後在 repo 根目錄執行 `make i18n-check`，確認格式正確。

## 新增一個語言

以日文 (`ja`) 為例：

1. 複製範本：`cp locales/ertangle.pot locales/ja.po`。
2. 修改檔頭的 `Language: ja` 與 `Plural-Forms`，並拿掉檔頭的 `#, fuzzy`。
3. 在 `crates/ertangle-core/src/i18n.rs` 註冊：
   - `Language` 加一個成員 (例如 `Ja`)。
   - `LANGUAGES` 加一列 `(Language::Ja, "ja", "日本語")`，顯示名稱用該語言自己的寫法。
   - `Language::parse`、`code`、`po` 與 `current` 加上對應分支，`po` 以 `include_str!("../../../locales/ja.po")` 內嵌。
   - `catalog` 加一個 `OnceLock`。
4. 翻譯 `ja.po`，執行 `make i18n-check` 與 `cargo test`。

複數形目前只支援兩種形式 (`n != 1`)。要加入規則不同的語言時，需要擴充 `i18n.rs` 的 `ntranslate`。

## 佔位符

原文裡的 `{name}` 是執行時代入的值，翻譯裡必須保留**同樣名稱、同樣數量**的佔位符，順序可以依語言調整：

```po
msgid "已跳轉至 {label} ({id})"
msgstr "Jumped to {label} ({id})"
```

- 不要翻譯佔位符名稱 (`{label}` 不能改成 `{標籤}`)。
- `{{` 與 `}}` 代表字面的大括號，照抄即可。
- CI 會檢查每條翻譯的佔位符是否跟原文一致。

## 用語

- 專有名詞保留原文：session、worktree、daemon、model、effort、agent、prompt。
- 命令名稱 (`:model`、`:set`)、按鍵、設定檔鍵名不翻譯。
- 英文介面空間有限，用簡短的 CLI 風格；不加句點，除非原文是多句。
- 同一個原文在不同情境需要不同譯法時，程式碼會用 context 區分 (`.po` 裡的 `msgctxt`)，例如 `msgctxt "列表分隔"` 的 `"、"` 譯成 `", "`。

## 給開發者

- 使用者看得到的字串用 `tr!("原文 {name}", name = value)` 包起來，`use tr::tr;`。一律用具名佔位符，不要把一句話拆成片段再拼接，翻譯者才看得到完整句子。
- `{x:?}` 這類格式規格 `tr!` 不支援：先 `format!` 好再當參數傳入。
- `&'static str` 表格用 `ertangle_core::tr_noop!("原文")` 標記，顯示時用 `ertangle_core::tr_dyn!(s)` 翻譯。
- log (`tracing`)、測試斷言、給 agent 的 prompt (協作說明、交接簡報) 不翻譯。
- 新增或修改字串後執行 `make i18n-extract` 更新 `ertangle.pot` 與各 `.po`，再補上英文翻譯。修改原文會讓舊翻譯變成 fuzzy，CI 會擋下，要確認翻譯後拿掉 `#, fuzzy`。
- 抽取工具是 [`xtr`](https://crates.io/crates/xtr) (`cargo install xtr`) 與 GNU gettext 的 `msgmerge`。`xtr` 採 AGPL-3.0，只當開發工具執行，不加進 `Cargo.toml`，也不隨專案發佈。
