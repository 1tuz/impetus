#!/bin/sh
# Deterministic host_process fixture: initialize / operate(echo) / cancel / shutdown.
while IFS= read -r line; do
  case "$line" in
    *extension/initialize*)
      id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p' | head -1)
      [ -z "$id" ] && id=1
      printf '%s\n' '{"jsonrpc":"2.0","id":'"$id"',"result":{"protocol_version":1,"name":"host-process-echo","extension_api_version":1,"supported_ops":["echo","invoke"]}}'
      ;;
    *extension/operate*)
      id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p' | head -1)
      rid=$(printf '%s' "$line" | sed -n 's/.*"request_id":"\([^"]*\)".*/\1/p' | head -1)
      [ -z "$id" ] && id=1
      [ -z "$rid" ] && rid=unknown
      case "$line" in
        *'"op":"echo"'*|*'\"op\":\"echo\"'*)
          printf '%s\n' '{"jsonrpc":"2.0","id":'"$id"',"result":{"request_id":"'"$rid"'","op":"echo","data":{"ok":true,"fixture":"host-process-echo"}}}'
          ;;
        *)
          printf '%s\n' '{"jsonrpc":"2.0","id":'"$id"',"error":{"code":-32013,"message":"unsupported op"}}'
          ;;
      esac
      ;;
    *extension/cancel*)
      id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p' | head -1)
      [ -z "$id" ] && id=1
      printf '%s\n' '{"jsonrpc":"2.0","id":'"$id"',"result":null}'
      ;;
    *extension/shutdown*)
      id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p' | head -1)
      [ -z "$id" ] && id=1
      printf '%s\n' '{"jsonrpc":"2.0","id":'"$id"',"result":null}'
      exit 0
      ;;
  esac
done
