---
name: taiwanese-tech-writing
description: >-
  Technical writing rules and terminology standards for Traditional Chinese
  (Taiwan, zh-TW). Must be followed whenever creating, editing, or reviewing
  documentation, code comments, commit messages, or user interfaces in
  Traditional Chinese. Enforces half-width parentheses, proper spacing, and
  standard Taiwanese software engineering vocabulary.
---

# 繁體中文 (台灣，zh-TW) 技術寫作與用語規範

本規範定義專案內撰寫繁體中文文件、程式碼註解、UI 顯示文字、Commit Message 與各類技術文件時**必須遵守**的格式與用語標準。

---

## 1. 括號與標點符號規範

### 1.1 括號規範 (重要)
- **一律使用半形括號 `(` 與 `)`**，**嚴格禁止使用全形括號「（」與「）」**。
- **半形括號間距**：
  - 括號內側緊鄰文字，不留空格：`(如預期)`，而非 `( 如預期 )`。
  - 括號外側若緊鄰中文字元，建議在左括號前保留一個半形空格，以維持版面節奏：`終端機原生 (Terminal-native)`。
  - 在 Markdown 連結語法中維持標準半形括號：`[標題](路徑)`。

### 1.2 標點符號
- **全形標點**：
  - 逗號：全形 `，` (不用半形 `,` 作為中文句號分割)
  - 句號：全形 `。` (不用半形 `.`)
  - 頓號：全形 `、` (列舉項目時使用)
  - 冒號與分號：說明文字時使用全形 `：` 與 `；`；若是指令、快速鍵或程式語法，則維持半形 `:` 與 `;`。
  - 引號：優先使用中文直角引號「」與『』，不使用英文引號 `"` 或 `'` 包裹中文句子。
- **反引號與程式符號**：
  - 檔名、路徑、CLI 指令、變數名稱、鍵位、環境變數等一律使用半形反引號包裹，例如 `` `termade` ``、`` `~/.config` ``。

### 1.3 中英混排間距 (盤古之白)
- 中文字與英文字母、阿拉伯數字、行內程式碼之間，**必須保留一個半形空格**。
  - ✅ 正確：`在 Rust 專案中執行 cargo test`
  - ❌ 錯誤：`在Rust專案中執行cargo test`
- 標點符號與相鄰文字之間**不加空格**。
  - ✅ 正確：`完成後，系統會回報。`
  - ❌ 錯誤：`完成後 ， 系統會回報 。`

---

## 2. 台灣常用技術用語對照表

所有技術文件與介面文字必須統一採用台灣軟體工程常用語，嚴格禁止非台灣習慣用語：

### 2.1 系統架構與環境

| 台灣標準用語 (zh-TW) | 避免用語 (非台灣用語) | 英文對應 (EN) |
| --- | --- | --- |
| **伺服器** | 服務端、服務器 | Server |
| **用戶端 / 客戶端** | 客戶端 (依情境) | Client |
| **終端機** | 終端、終端機 (不宜簡化為終端) | Terminal |
| **記憶體** | 內存 | Memory (RAM) |
| **快取** | 緩存 | Cache |
| **處理程序 / 行程 / 程序** | 進程 | Process |
| **執行緒** | 線程 | Thread |
| **非同步** | 異步 | Asynchronous |
| **並發 / 併發** | 併發 | Concurrency |
| **登入 / 登出** | 登錄 / 註銷 | Login / Logout |
| **通訊協定 / 協定** | 協議 | Protocol |
| **重試** | 重試 (保持一致) | Retry |

### 2.2 軟體開發與 Git

| 台灣標準用語 (zh-TW) | 避免用語 (非台灣用語) | 英文對應 (EN) |
| --- | --- | --- |
| **程式碼** | 代碼、源碼 | Code |
| **寫程式 / 開發** | 編程 | Programming / Coding |
| **專案** | 項目 | Project |
| **建立** | 創建 | Create |
| **執行** | 運行 | Run / Execute |
| **預設** | 默認 | Default |
| **透過** | 通過 (指藉由/via時) | Via / Through |
| **支援** | 支持 | Support |
| **相依性** | 依賴 | Dependency |
| **介面** | 接口 | Interface |
| **函式 / 函數** | 方法 (若是頂層函式時) | Function |
| **呼叫** | 調用 | Call / Invoke |
| **參數** | 參量 | Parameter / Argument |
| **模組 / 套件** | 庫、模塊 | Module / Package |
| **函式庫** | 庫 | Library |
| **物件** | 對象 | Object |
| **封裝** | 封裝 | Encapsulate |
| **自動完成** | 自動補全 / 自動補齊 | Completions |
| **二進位檔 / 執行檔** | 二進制文件 | Binary / Executable |
| **腳本** | 腳本 | Script |
| **分支** | 分支 | Branch |
| **合併** | 合併 | Merge |
| **審查 / 審核 / 核准** | 審批 | Review / Approve |

### 2.3 資料結構與型別

| 台灣標準用語 (zh-TW) | 避免用語 (非台灣用語) | 英文對應 (EN) |
| --- | --- | --- |
| **資料** | 數據 | Data |
| **資料庫** | 數據庫 | Database |
| **字串** | 字符串 | String |
| **陣列** | 數組 | Array |
| **佇列** | 隊列 | Queue |
| **堆疊** | 棧 | Stack |
| **字典 / 映射** | 字典 / 映射 | Map / Dictionary |
| **結構** | 結構體 | Struct |

### 2.4 使用者介面與操作

| 台灣標準用語 (zh-TW) | 避免用語 (非台灣用語) | 英文對應 (EN) |
| --- | --- | --- |
| **快速鍵 / 快捷鍵** | 熱鍵、快捷方式 | Shortcut / Hotkey |
| **剪貼簿** | 剪切板、剪貼板 | Clipboard |
| **游標** | 光標 | Cursor |
| **螢幕** | 屏幕 | Screen |
| **視窗** | 窗口 | Window |
| **選取** | 反白、選中 | Select |
| **拖曳** | 拖拽 | Drag |
| **滾輪** | 滾輪 | Scroll wheel |
| **儲存** | 保存 | Save |
| **彈出視窗 / 浮層** | 彈窗 | Pop-up / Overlay |
| **訊息** | 信息 (指 message 時) | Message |
| **資訊** | 信息 (指 information 時) | Information |
| **連結** | 連接 (指 URL/Link 時) | Link |
| **列 / 行** | 行 / 列 (直為行、橫為列，或程式碼行) | Row / Line / Column |

---

## 3. 執行檢驗清單

在送出任何包含繁體中文的文件或修改時，請自檢以下四點：
1. [ ] 是否存在任何全形括號「（」或「）」？若有，全數轉換為半形 `(` 與 `)`。
2. [ ] 是否出現非台灣用語 (如「內存」、「創建」、「默認」、「代碼」、「運行」、「依賴」等)？若有，依對照表修正。
3. [ ] 中文與英文、數字之間是否有適度保留空格 (盤古之白)？
4. [ ] 程式碼符號與鍵位是否正確使用反引號標記？
