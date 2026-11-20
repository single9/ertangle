# T57 — 專案改名：termade → ertangle (ER=EPR)

優先度：P1　範圍：全專案 (crate、CLI、MCP、設定、文件)　相依：—

Status: done

## 問題

`termade` 在搜尋引擎會被自動改正成高爾夫球品牌 TaylorMade，另有近似名稱的競品 `termide`，不利於被找到。

## 決定

- 新名稱 `ertangle` (念作 er-tangle)，由 ER (愛因斯坦-羅森橋) 與 entangle (糾纏) 組成，典故為 Maldacena 與 Susskind 的 ER=EPR 猜想：每個 agent 在自己的宇宙 (worktree)，透過糾纏 (MCP 訊息匯流排) 連通。與作者另一個專案 erbridge (Einstein-Rosen bridge) 成為同一系列。
- crates.io、npm 與 GitHub 帳號 `ertangle` 皆未被使用，搜尋結果也沒有撞名。
- 曾考慮 `erepr`，因為不好念而放棄。

## 改名對照

| 舊 | 新 |
| --- | --- |
| crate `termade`、`termade-{core,protocol,daemon,tui,adapter}` | `ertangle`、`ertangle-{core,protocol,daemon,tui,adapter}` |
| 執行檔 / CLI `termade` | `ertangle` |
| 環境變數 `TERMADE_*` | `ERTANGLE_*` |
| 設定目錄 `~/.config/termade`、repo 內 `.termade/` | `~/.config/ertangle`、`.ertangle/` |
| 狀態目錄 `~/.local/state/termade`、執行期目錄 `$XDG_RUNTIME_DIR/termade` | `.../ertangle` |
| MCP 伺服器名稱 `termade`、工具 `termade_*` | `ertangle`、`ertangle_*` |
| skill `termade-ade` | `ertangle-ade` |
| 分支前綴 `termade/<session>` | `ertangle/<session>` |
| i18n `locales/termade.pot` | `locales/ertangle.pot` |

## 相容層 (保留至 v1.0)

- 環境變數：`ERTANGLE_X` 未設定時退回讀 `TERMADE_X`。
- 設定與狀態目錄：新目錄不存在而舊目錄存在時，啟動時把舊目錄搬到新位置 (rename，失敗則退回讀舊路徑)，並記錄一行 log；repo 內 `.termade/` 只讀取不搬移。
- `skill uninstall` 同時移除舊的 `termade-ade`。
- 舊的 `termade/*` 分支不處理。

## 不在範圍內

- `docs/tickets/` 舊票與 `CHANGELOG.md` 既有內容維持原樣 (歷史紀錄)。
- GitHub repo 改名、本機目錄改名、crates.io 佔名，由作者手動處理。

## 驗收

- `rg -i termade` 只剩：相容層程式碼與其測試、舊票、CHANGELOG 歷史、文件中的改名說明。
- `make pre-commit` 與 `cargo test --workspace` 通過。
- 相容層有單元測試 (環境變數退回、目錄搬移、搬移失敗退回)。
