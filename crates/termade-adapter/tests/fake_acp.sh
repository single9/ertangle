#!/bin/sh
# 假的 ACP agent (stdio JSON-RPC)：initialize、session/new、session/load、session/prompt。
# `session/load` 會先重播一段歷史再回應，用來驗證載入期間的輸出不會往上送。
# 環境變數 FAKE_ACP_NO_LOAD=1 時不宣告 loadSession。
load=true
[ -n "$FAKE_ACP_NO_LOAD" ] && load=false
update() {
  printf '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"%s\\n"}}}}\n' "$1"
}
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":1,"agentCapabilities":{"loadSession":%s}}}\n' "$id" "$load" ;;
    *'"method":"session/new"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"sessionId":"acp-new"}}\n' "$id" ;;
    *'"method":"session/load"'*)
      update "OLD HISTORY"
      printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id" ;;
    *'"method":"session/prompt"'*)
      update "fresh reply"
      printf '{"jsonrpc":"2.0","id":%s,"result":{"stopReason":"end_turn"}}\n' "$id" ;;
  esac
done
