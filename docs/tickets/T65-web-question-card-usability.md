# T65 — 改善網頁待回答問題卡片的可用性

Priority: P1　Scope: `ertangle-daemon`　Depends on: T64

Status: done

## Problem

網頁介面的待回答問題 (multiple-choice question) 幾乎無法使用，在 Chat 分頁尤其嚴重：

- 選項、自訂輸入框與送出按鈕被塞進同一個水平 `.btn-row`，`.btn-pill {flex:1}` 還把送出按鈕拉滿整列。
- 每次收到 state 事件 (Agent 輸出會持續串流) 都會重建 `innerHTML`，使用者已勾選的選項與輸入的自訂文字全部被清掉。
- sticky 橫幅放在 `<main>` 內，Chat 分頁的 `.tab-view {height:100%}` 會把輸入框擠出視窗外，長選項清單甚至蓋滿整個螢幕。
- 同一個 `group` 的多個問題只會顯示第一題，也沒有「1/N」提示。

## Implementation

- 版面改為垂直：標題與問題文字可換行 (不再 `word-break: break-all`)，選項為整列可點擊的卡片 (標籤粗體、說明另起一行、已選取狀態高亮、觸控高度 >= 44px)，自訂輸入獨立一行，送出按鈕靠右不拉伸。
- 以簽章 (`setCardHtml` 比對 `dataset.sig`) 控制重繪，只有問題集合改變才重建，勾選與自訂文字在 state 事件間保留。
- Chat 分頁且問題屬於目前 Session 時，在 `#chat-question-slot` 內嵌顯示 (位於逐字稿與輸入框之間，`max-height: 50vh` 並可內部捲動)，隱藏全域橫幅；其他情況橫幅改為單行摘要加「回答」按鈕，按下後切換到該 Session 的 Chat。
- 多題顯示「1/N」；同一 `group` 的問題一起顯示，一個送出鈕對每題各送一次 `answer_question`。
- 同一 `group` 至少要回答一題才能送出；未回答的同組問題送出 `answers: []` (視為略過)。Daemon 不再於第一題回答後自動清除同組其他問題，每題只由自己的 `answer_question` 解決，避免後續答案找不到等待者而遺失。
- 單選仍須按送出 (避免誤觸)，自訂輸入框按 Enter 即送出。仍送出選項 label，由 daemon 對應 value。

## Acceptance criteria

- 選項垂直排列且在手機上容易點選。
- 串流輸出期間已選取的選項與自訂文字不會消失。
- Chat 分頁中橫幅不會把輸入框推出螢幕。
- `make pre-commit` 與 `cargo test -p ertangle-daemon` 通過。
