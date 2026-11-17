# T42 — TUI 設定選單的 `inject_skill` 開關與 `:skill` 指令

優先度：P3　範圍：`termade-tui`　相依：T38

Status: ready-for-agent

## 問題

T38 加入了 `termade-ade` skill 與 `[collab] inject_skill` 設定，但只能透過 CLI (`termade skill install`) 或手動編輯 `config.toml` 使用，TUI 使用者不容易發現這個可選功能。

## 證據

- `crates/termade-daemon/src/config.rs:148` 有 `inject_skill: Option<bool>`，`collab.rs:164` 依此附加 skill 內文。
- `crates/termade-tui/src/settings.rs` 已有寫入全域 `config.toml` 的設定項目模式 (例如 `Item::WebCompanion`)。
- `src/cmd/skill.rs` 的安裝邏輯在執行檔 crate，TUI 無法直接呼叫。

## 分析邏輯

- 開關寫入全域設定即可，新建立的 session 生效 (協作說明在 spawn 時組出)。
- 安裝路徑邏輯需要搬到 `termade-daemon::skill`，讓 CLI 與 TUI 共用，避免兩份實作。

## 實作方式

- `settings.rs` 新增 `Item::InjectSkill`，在開／關之間切換並寫入 `[collab] inject_skill`。
- 安裝／移除邏輯移到 `termade-daemon::skill`；TUI 新增 `:skill [install|uninstall|show] [claude|agents] [global]`，預設安裝到目前專案的 `.claude/skills`。
- 命令提示與 i18n 同步更新。

## 驗收條件

- `:settings` 可切換「注入 termade-ade skill」，`config.toml` 對應更新。
- `:skill install` 在目前專案建立 `.claude/skills/termade-ade/SKILL.md`，`:skill uninstall` 移除。
- `cargo test`、`make pre-commit` 通過。
