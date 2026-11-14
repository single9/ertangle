#!/bin/sh
# 假的 `codex app-server`。FAKE_MODE：
#   auth    回合送出 `error` 通知 (unauthorized)後 `turn/completed` 帶同一個 error
#   plain   同上但是普通錯誤
#   die     往 stderr 印幾行後以代碼 3 結束 (握手前就掛掉)
#   flood   握手前先往 stderr 灌大量輸出，之後正常回合，最後以代碼 1 結束
case "$FAKE_MODE" in
  die)
    i=0
    while [ $i -lt 30 ]; do echo "boom line $i" >&2; i=$((i + 1)); done
    printf '\033[31mERROR\033[0m final failure\n' >&2
    exit 3
    ;;
  flood)
    yes "noise noise noise noise noise noise noise noise noise noise noise noise" | head -n 20000 >&2
    ;;
esac
msg="Your access token could not be refreshed because your refresh token was revoked."
info="unauthorized"
if [ "$FAKE_MODE" = plain ]; then msg="model exploded"; info="other"; fi
err='{"message":"'"$msg"'","codexErrorInfo":"'"$info"'","additionalDetails":null}'
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*) printf '{"id":%s,"result":{}}\n' "$id" ;;
    *'"method":"thread/start"'*) printf '{"id":%s,"result":{"thread":{"id":"thr-1"},"model":"fake"}}\n' "$id" ;;
    *'"method":"account/rateLimits/read"'*) printf '{"id":%s,"error":{"code":-32601,"message":"nope"}}\n' "$id" ;;
    *'"method":"turn/start"'*)
      printf '{"id":%s,"result":{"turn":{"id":"t1","error":null}}}\n' "$id"
      printf '{"method":"turn/started","params":{"turn":{"id":"t1"}}}\n'
      if [ "$FAKE_MODE" = flood ]; then
        printf '{"method":"turn/completed","params":{"turn":{"id":"t1","error":null}}}\n'
        exit 1
      fi
      printf '{"method":"error","params":{"error":%s,"willRetry":false}}\n' "$err"
      printf '{"method":"turn/completed","params":{"turn":{"id":"t1","status":"failed","error":%s}}}\n' "$err"
      ;;
  esac
done
