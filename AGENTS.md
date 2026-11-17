# Agent Guidelines for ade-rust / termade

## Git Commit Convention

All git commit messages **MUST** be written in **English** following standard Conventional Commits format (e.g. `feat(scope): description`, `fix(scope): description`, `refactor(scope): description`, `chore: description`, etc.). Do **NOT** write git commit messages in Chinese.

## Inter-Agent Communication Protocol

All inter-agent messages sent via `termade send` / `termade_send` or `termade ask` / `termade_ask` **MUST** use a concise, structured **English** protocol to minimize token consumption and prevent unproductive turn wakeups:

1. **Structured Message Formats**:
   - Task completion: `TASK_DONE <task_id> <commit_or_short_summary>`
   - Task blocked: `TASK_BLOCKED <task_id> <reason>`
   - Question / query: `QUERY: <concise_question>`
   - Direct answer: `ANSWER: <direct_answer>`
   - Optional acknowledgment (omit unless required): `ACK`

2. **Zero Pleasantries Policy & Terminal State**:
   - Strictly forbid pleasantries, courtesy ping-pong, and polite sign-offs (e.g. `"Thank you"`, `"You're welcome"`, `"Goodbye"`, `"辛苦了"`, `"合作愉快"`, `"祝一切順利"`).
   - Every message sent to another session wakes that agent for a full turn and re-injects its entire conversation context. Never reply merely to say thanks or goodbye.
   - **Terminal State**: Once a worker sends `TASK_DONE`, it **MUST** terminate its turn immediately. Once a coordinator receives `TASK_DONE`, it should proceed directly with verification, merging, or task board updates without sending a courtesy reply.

## Pre-commit Verification Checklist (Commit 前檢查清單)

Before committing changes or pushing to git, all agents and contributors **MUST** verify the following checks locally to ensure CI will not fail:

1. **Code formatting**: Run `cargo fmt` to automatically format all code (`cargo fmt --check` must pass).
2. **Clippy linter**: Run `cargo clippy --all-targets -- -D warnings` and fix all warnings.
3. **i18n catalog synchronization**:
   - If any user-facing strings or `tr!("...")` are added, modified, or removed:
     - Run `make i18n-extract` to update `locales/termade.pot` and `locales/en.po`.
     - Fill in the English translations in `locales/en.po` (no empty `msgstr` or fuzzy tags).
     - Verify with `make i18n-check`.
4. **Automated pre-commit hook**:
   - Developers can run `make setup-hooks` to automatically install `.git/hooks/pre-commit`, which enforces formatting, clippy, and i18n checks on every `git commit`.
5. **One-command verification**:
   - Run `make pre-commit` to automatically format code, check clippy warnings, and verify i18n catalogs in a single step.

## Traditional Chinese Documentation & Copywriting Rules (台灣繁體中文規範)

Whenever generating, modifying, or reviewing documentation, comments, or user-facing text in **Traditional Chinese (zh-TW)**, you **MUST** strictly adhere to the following rules (detailed in `.agents/skills/taiwanese-tech-writing/SKILL.md`):

1. **No Full-Width Parentheses**:
   - Always use half-width parentheses `(` and `)`.
   - **Never** use full-width parentheses `（` and `）`.
   - Maintain a space before the opening parenthesis when adjacent to Chinese characters, e.g. `終端機原生 (Terminal-native)`.

2. **Strict Taiwanese Terminology (台灣慣用軟體工程用語)**:
   - Use standard Taiwanese terms; avoid mainland Chinese technical terms:
     - 伺服器 (not 服務器 / 服務端)
     - 程式碼 (not 代碼 / 源碼)
     - 專案 (not 項目)
     - 終端機 (not 終端)
     - 記憶體 (not 內存)
     - 快取 (not 緩存)
     - 處理程序 / 行程 (not 進程)
     - 執行緒 (not 線程)
     - 非同步 (not 異步)
     - 建立 (not 創建)
     - 執行 (not 運行)
     - 預設 (not 默認)
     - 透過 (not 通過)
     - 支援 (not 支持)
     - 相依性 (not 依賴)
     - 介面 (not 接口)
     - 函式庫 / 模組 (not 庫 / 模塊)
     - 資料 / 資料庫 (not 數據 / 數據庫)
     - 陣列 (not 數組)
     - 字串 (not 字符串)
     - 佇列 (not 隊列)
     - 快速鍵 / 快捷鍵 (not 熱鍵 / 快捷方式)
     - 剪貼簿 (not 剪切板 / 剪貼板)
     - 游標 (not 光標)
     - 視窗 / 螢幕 (not 窗口 / 屏幕)
     - 拖曳 (not 拖拽)
     - 選取 (not 反白 / 選中)
     - 登入 (not 登錄)
     - 儲存 (not 保存)
     - 審查 / 審核 / 核准 (not 審批)
     - 訊息 / 資訊 (not 信息)
     - 連結 (not 連接)
     - 自動補齊 / 自動完成 (not 自動補全)
     - 二進位檔 / 執行檔 (not 二進制文件)

3. **Typography (中英混排與標點)**:
   - Add a half-width space between Chinese characters and English letters, numbers, or inline code blocks (盤古之白).
   - Use full-width commas `，` and periods `。` for Chinese sentences.
   - Use Chinese quotation marks 「」 and 『』 instead of ASCII quotes `""` for Chinese text.

## Agent skills

### Issue tracker

Tickets are local markdown files, one per ticket, at `docs/tickets/TNN-<slug>.md` with an index table in `docs/tickets/README.md`. See `docs/agents/issue-tracker.md`.

### Triage labels

Default vocabulary (`needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`), recorded as a `Status:` line in each ticket. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: one `CONTEXT.md` and `docs/adr/` at the repo root (created lazily). See `docs/agents/domain.md`.
