#!/bin/sh
# 假的 claude stream-json agent：每收到一行 stdin 就回一段輸出與 result。
# 帶 `--resume <id>` 啟動時，`init` 回報同一個 id，並多輸出一行 `resumed <id>`。
sid="fake-new"
resumed=""
while [ $# -gt 0 ]; do
  if [ "$1" = "--resume" ]; then
    sid="$2"
    resumed="resumed $2\\n"
    shift
  fi
  shift
done
while IFS= read -r line; do
  printf '%s\n' '{"type":"system","subtype":"init","session_id":"'"$sid"'","model":"fake-model","tools":[]}'
  printf '%s\n' '{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"'"$resumed"'pong from fake\n"}}}'
  printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"total_cost_usd":0.25,"usage":{"input_tokens":3,"output_tokens":4}}'
done
