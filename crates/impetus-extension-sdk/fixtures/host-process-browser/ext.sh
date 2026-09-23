#!/bin/sh
# Deterministic host_process fixture: browser/health + browser/negotiate.
while IFS= read -r line; do
  case "$line" in
    *extension/initialize*)
      id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p' | head -1)
      [ -z "$id" ] && id=1
      printf '%s\n' '{"jsonrpc":"2.0","id":'"$id"',"result":{"protocol_version":1,"name":"host-process-browser","extension_api_version":1,"supported_ops":["browser/health","browser/negotiate"]}}'
      ;;
    *extension/operate*)
      id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p' | head -1)
      rid=$(printf '%s' "$line" | sed -n 's/.*"request_id":"\([^"]*\)".*/\1/p' | head -1)
      [ -z "$id" ] && id=1
      [ -z "$rid" ] && rid=unknown
      case "$line" in
        *'"op":"browser/health"'*|*'\"op\":\"browser/health\"'*)
          # BrowserHealthStatus::Available — serde tag = "status"
          printf '%s\n' '{"jsonrpc":"2.0","id":'"$id"',"result":{"request_id":"'"$rid"'","op":"browser/health","data":{"status":"available","provider_id":"host-process-browser","capabilities":["health","negotiate"]}}}'
          ;;
        *'"op":"browser/negotiate"'*|*'\"op\":\"browser/negotiate\"'*)
          pv=$(printf '%s' "$line" | sed -n 's/.*"protocol_version":"\([^"]*\)".*/\1/p' | head -1)
          [ -z "$pv" ] && pv=unknown
          if [ "$pv" = "0.1" ]; then
            printf '%s\n' '{"jsonrpc":"2.0","id":'"$id"',"result":{"request_id":"'"$rid"'","op":"browser/negotiate","data":{"protocol_version":"'"$pv"'","compatible":true,"reason":"fixture accepts 0.1"}}}'
          else
            printf '%s\n' '{"jsonrpc":"2.0","id":'"$id"',"result":{"request_id":"'"$rid"'","op":"browser/negotiate","data":{"protocol_version":"'"$pv"'","compatible":false,"reason":"fixture only accepts 0.1"}}}'
          fi
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
