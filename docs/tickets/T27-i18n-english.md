# T27 — 多國語系：以 gettext `.po` 提供英文介面（可讓社群協助翻譯）

優先度：P2　範圍：`termade-tui`、`termade-daemon`、`termade-adapter`、`src/`、CI　相依：—

Status: needs-triage（規劃，尚未動工）

## 目標與範圍

- 只做英文；格式用 gettext `.po`，這樣 Weblate、Crowdin、Poedit 都能直接接，社群日後可以自行加其他語言，不必改程式碼。
- 原始碼裡的繁體中文字串就是 `msgid`（gettext 慣例，來源語言即 msgid）。英文放在 `locales/en.po` 的 `msgstr`。
- 預設語言維持繁體中文。沒有翻譯的字串退回原文（中文），不會出現空字串。

## 範圍內與範圍外

| 內容 | 處理 |
| --- | --- |
| TUI 介面：面板、說明、命令列說明、選單、狀態列（約 310 個字串） | 翻譯 |
| daemon／CLI 回給使用者的訊息、錯誤（約 300 個字串） | 翻譯 |
| 協作說明（`collab.rs`，約 33 個字串） | 先不翻譯（見下方「待決定」） |
| 命令名稱（`:model`、`:set`）、設定檔鍵名、協定欄位 | 不翻譯 |
| log／tracing、測試斷言、程式碼註解 | 不翻譯 |

## 技術選擇

用 [`tr`](https://crates.io/crates/tr) 0.1.11（MIT）：

- `tr!("來源文字", name = value)` 巨集，語意對應 gettext；有 `po-translator` feature，以純 Rust 的 `rspolib` 直接讀 `.po`，不需要 C 的 `gettext-rs`（避免影響 macOS universal2 與 Linux 的交叉編譯），也不需要先編 `.mo`。
- `.po` 用 `include_str!` 內嵌進執行檔，維持單一執行檔發佈，不依賴執行時期的檔案路徑。
- 抽取字串用 [`xtr`](https://crates.io/crates/xtr)（`tr` 作者的 `xgettext` 替代品）產生 `locales/termade.pot`。`xtr` 是 AGPL-3.0，只當開發工具執行、不連結進專案，也不隨專案發佈；CI 與 `Makefile` 只呼叫它，不把它加進 `Cargo.toml`。
- 語言偵測用 `sys-locale`（MIT／Apache-2.0），或直接讀 `LC_ALL`／`LC_MESSAGES`／`LANG`，不需要額外相依性時就不加。

以上 crate 資訊是 2026-11-14 以 `cargo info` 查到的 feature 與授權；`po-translator` 實際能不能載入帶複數形或 context 的 `.po`、`tr!` 對具名與位置參數的支援範圍，都還沒實測，列在步驟 1 驗證。

## 語言選擇

優先順序：`config.toml` 的 `[tui] language`（`auto`、`zh-TW`、`en`）→ 環境變數 `LC_ALL`、`LC_MESSAGES`、`LANG` → 預設 `zh-TW`。設定面板加一列「介面語言」，用既有的清單選擇器切換，切換後立即生效，不必重啟。

daemon 是共用的常駐行程。這張票先讓 daemon 回傳的訊息跟「daemon 啟動時的語言」走；需要依連線的 client 語言回覆時再另開票。

## 實作步驟

1. 驗證原型：只改 `settings.rs`（約 20 個字串），確認 `tr!` 搭配 `po-translator` 可以讀內嵌的 `en.po`、具名參數與 `{}` 位置參數都能用、缺譯時退回原文。這一步失敗就改用 `gettext` crate 加 build 時編 `.mo`。
2. 建立基礎設施：`locales/en.po`、`locales/termade.pot`、`i18n.rs`（語言偵測、切換、載入）、`Makefile` 的 `make i18n-extract`（`xtr` 產生 `.pot` 並用 `msgmerge` 更新各 `.po`）。
3. 分批把字串改成 `tr!`：先 TUI（`commands.rs`、`help.rs`、`draw.rs`、`menu.rs`、`input.rs`、`settings.rs`），再 daemon 與 CLI 的使用者訊息。每批各自一個 commit，方便審查。
4. `format!("已建立 session {id}")` 這類組句改成 `tr!("已建立 session {id}", id = id)`，不要把句子拆成片段拼接，翻譯者才看得到完整句子。命令說明表 `COMMANDS` 是 `&'static str`，要改成執行時呼叫 `tr!`。
5. 英文翻譯：先以人工寫，專有名詞（session、worktree、daemon、model、effort）保持原文。
6. CI：
   - 檢查 `.pot` 沒有過期（重新抽取後跟已提交的相同）。
   - 檢查 `en.po` 沒有 fuzzy 項目、沒有遺漏的 `msgid`。
   - 檢查每個 `msgstr` 的 `{placeholder}` 與 `msgid` 一致（翻譯者最容易弄壞的地方）。
7. 文件：`docs/` 新增「翻譯指南」，說明怎麼用 Poedit 或 Weblate 編輯 `.po`、怎麼新增一個語言（複製 `.pot` 為 `locales/<語言>.po`，在 `i18n.rs` 註冊）、佔位符規則。

## 風險與注意事項

- 中文 `msgid` 一旦改字（例如修錯字），所有語言的翻譯都會對不上而退回原文。改字要同時更新 `.po`，`make i18n-extract` 加 `msgmerge` 會把舊項目標成 fuzzy，由 CI 擋下。
- 長度：英文通常比中文長，TUI 的欄寬、說明面板與狀態列要實際看過，不能只靠測試。
- 測試目前有不少斷言直接比對中文字串；預設語言維持 zh-TW 時不受影響，測試不要依賴環境變數的語言。
- 協作說明同時是給 agent 看的 prompt，翻成英文會連帶影響 agent 的回覆語言與行為，品質要另外評估。

## 待決定

- 協作說明是否也跟介面語言走，或固定一種語言。建議本票先不動。
- 是否接 Weblate（託管版對開源專案免費）：等英文完成、有社群需求再決定。

## 驗收條件

- `[tui] language = "en"` 時，TUI 與 CLI 的使用者訊息全為英文，沒有殘留的中文（協作說明除外）。
- 預設與 `zh-TW` 的行為跟現在完全相同，既有測試全過。
- 設定面板能切換語言並立即生效。
- 缺譯的字串退回中文，不 panic、不顯示空白。
- CI 檢查 `.pot` 同步、佔位符一致、無 fuzzy。
- 翻譯指南文件完成，社群可以只靠 `.po` 加入新語言。
