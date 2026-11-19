# T53：額度警告 (`QUOTA_WARN` / `QUOTA_NOTICE`) 依 Agent 種類精準分發與明確化訊息語意

Status: done

## 問題

當某一種 agent（例如 `claude`）的額度用量達到警告門檻 (`90%`) 時，目前系統存在兩個問題：

1. **無差別廣播喚醒所有 Coordinator**：
   - 在 `crates/termade-daemon/src/quota_watch.rs` (`warn_if_quota_high`) 中，`recipients` 條件為：
     ```rust
     s.status.is_live() && (s.kind.id() == quota.agent || s.role == termade_core::Role::Coordinator)
     ```
     只要是執行中的 Coordinator，即使該 Coordinator 本身是 `antigravity`，且旗下完全沒有任何 `claude` 的活躍子 session，也會收到 `QUOTA_WARN claude 90%` 並被強制喚醒一輪對話（消耗整個對話上下文的 input tokens）。
2. **警告文案與協作指令未指明「僅針對該 `<agent>` 限制派生」**：
   - `quota_warn_body` 目前內容為：
     `QUOTA_WARN {agent} {used:.0}%: stop spawning new workers, checkpoint/commit work now, report status.`
   - `collab.rs` 與 `SKILL.md` 寫道：
     `when QUOTA_WARN arrives, stop spawning and have workers commit and report.`
   - 這導致一個 `antigravity` (額度僅 29%) 的 Coordinator 收到 `QUOTA_WARN claude 90%` 時，誤以為整個系統禁止再派生任何 worker，而停止派生 `antigravity` 或 `codex` 的 worker。

## 設計與實作方式

### 1. 收斂 `QUOTA_WARN` 與 `QUOTA_NOTICE` 的收件者範圍 (`crates/termade-daemon/src/quota_watch.rs`)

新增純函式 `should_notify_quota(session: &termade_core::Session, all_sessions: &std::collections::BTreeMap<SessionId, termade_core::Session>, agent: &str) -> bool`：
- 僅在 `session.status.is_live()` 時考慮：
  1. **自身即為該 Agent**：`session.kind.id() == agent`（適用於 `QUOTA_WARN`；而在 `QUOTA_NOTICE` 時則要求 `session.role == Role::Coordinator && session.kind.id() == agent`）。
  2. **身為 Coordinator 且旗下有該 Agent 的活躍子孫 session**：`session.role == Role::Coordinator`，且 `all_sessions` 中存在任一 `child` 滿足 `child.status.is_live() && child.kind.id() == agent` 且 `child` 在此 Coordinator 的子樹中（即沿 `parent` 往上走可到達 `session.id`，或在同一專案下由其管理）。
- 如此一來，一個純 `antigravity` 團隊（Coordinator 與 Workers 皆為 `antigravity`）不會因為背景或其他專案的 `claude` 額度達 90% 而被無故喚醒。

### 2. 明確化 `quota_warn_body` 與 `quota_notice_body` 的 Agent 針對性 (`crates/termade-daemon/src/quota_watch.rs`)

- 將 `quota_warn_body(agent: &str, used: f32)` 更新為：
  ```rust
  pub(crate) fn quota_warn_body(agent: &str, used: f32) -> String {
      format!(
          "QUOTA_WARN {agent} {used:.0}%: stop spawning new {agent} workers (other agents with available quota may still be used); if you or your active workers run on {agent}, checkpoint/commit work now and report status."
      )
  }
  ```
- 將 `quota_notice_body(agent: &str, used: f32)` 更新為：
  ```rust
  pub(crate) fn quota_notice_body(agent: &str, used: f32) -> String {
      format!(
          "QUOTA_NOTICE {agent} {used:.0}% (provider warning): avoid spawning new {agent} workers unless necessary (prefer other agents with available quota); finish current {agent} tasks and commit progress."
      )
  }
  ```

### 3. 更新協作說明與 Skill 文件 (`crates/termade-daemon/src/collab.rs`, `crates/termade-daemon/src/skills/termade-ade/SKILL.md`)

- 在 `crates/termade-daemon/src/collab.rs` (第 84 行附近)：
  將 `when \`QUOTA_WARN\` arrives, stop spawning and have workers commit and report.` 改為：
  `when \`QUOTA_WARN <agent>\` arrives, stop spawning \`<agent>\` workers (other agents with available quota can still be used) and have active \`<agent>\` sessions commit and report.`
- 在 `crates/termade-daemon/src/skills/termade-ade/SKILL.md` (第 29 行)：
  將 `5. On \`QUOTA_WARN\`: stop spawning, tell workers to commit and report, then finish inline or wait for reset.` 改為：
  `5. On \`QUOTA_WARN <agent>\`: stop spawning \`<agent>\` workers (switch to another agent with available quota if needed) and have active \`<agent>\` workers commit and report.`

## 驗收條件

1. `crates/termade-daemon/src/quota_watch.rs` 單元測試驗證：
   - `quota_warn_body` 與 `quota_notice_body` 明確包含針對 `{agent}` 的指引與其他可用 agent 的說明。
   - `should_notify_quota` 測試：
     - `claude` 額度告警時，`claude` worker 與 `claude` coordinator 會收到通知。
     - 旗下有活躍 `claude` worker 的 `antigravity` coordinator 會收到通知。
     - 自身為 `antigravity` 且旗下僅有 `antigravity` worker（或無 worker）的 coordinator **不會**收到 `claude` 的 `QUOTA_WARN` / `QUOTA_NOTICE`。
2. `make pre-commit` 與 `cargo test` 全數通過。
