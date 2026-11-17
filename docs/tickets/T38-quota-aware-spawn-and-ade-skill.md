# T38 — 額度感知的 spawn、即時額度警告與可安裝的 termade-ade skill

優先度：P2　範圍：`termade-daemon`、`termade` CLI、`docs/`　相依：T20, T37

Status: done

## 問題

agent 自行 spawn 子 session 會快速消耗 token，也可能在額度用盡時讓工作被中斷。

## 做法

1. **額度感知 spawn**：agent 自行建立 session 時檢查該 agent 的額度。達到 `[handoff] warn_percent` (預設 90%) 或已滿時，改用 `[handoff] fallback` 中可用的 agent；沒有可用者就以英文錯誤拒絕 (含重置時間)。成功的回覆一律附上簡短額度摘要，例如 `quota claude 72% (5h window)`。
2. **同時子 session 上限**：新增 `[agent_spawn] max_live_children` (預設 3)，沿用既有 `max_children` 檢查；`[limits] max_children` 有設時以它為準。
3. **即時警告**：額度越線時除了 SessionNote，也透過匯流排送 `QUOTA_WARN <agent> <used>%: ...` 給執行中的 coordinator 與該 agent 的 session，可喚醒 `termade_wait`。`termade_whoami` 另外回傳 `quotas`。
4. **省 token 指引**：coordinator 協作說明與 `termade_spawn` 工具描述加入精簡原則。
5. **termade-ade skill**：內嵌 SKILL.md，提供 `termade skill install|uninstall|show`；`[collab] inject_skill = true` 時把內文附加到協作說明 (預設關閉)。

## 驗收

- `check_spawn_quota` 的放行、改用 fallback、拒絕三種情形有單元測試。
- skill 安裝與移除路徑 (claude／agents／codex) 以暫存目錄測試。
