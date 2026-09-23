#!/bin/sh
# Deterministic host_process fixture: coding/* LSP operate ops.
while IFS= read -r line; do
  case "$line" in
    *extension/initialize*)
      id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p' | head -1)
      [ -z "$id" ] && id=1
      printf '%s\n' '{"jsonrpc":"2.0","id":'"$id"',"result":{"protocol_version":1,"name":"host-process-lsp","extension_api_version":1,"supported_ops":["coding/definition","coding/hover","coding/diagnostics","coding/symbols","coding/references","coding/cancel"]}}'
      ;;
    *extension/operate*)
      id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p' | head -1)
      rid=$(printf '%s' "$line" | sed -n 's/.*"request_id":"\([^"]*\)".*/\1/p' | head -1)
      [ -z "$id" ] && id=1
      [ -z "$rid" ] && rid=unknown
      case "$line" in
        *'"op":"coding/definition"'*|*'\"op\":\"coding/definition\"'*)
          printf '%s\n' '{"jsonrpc":"2.0","id":'"$id"',"result":{"request_id":"'"$rid"'","op":"coding/definition","data":[{"path":"src/lib.rs","range":{"start":{"line":10,"character":0},"end":{"line":10,"character":3}}}]}}'
          ;;
        *'"op":"coding/references"'*|*'\"op\":\"coding/references\"'*)
          printf '%s\n' '{"jsonrpc":"2.0","id":'"$id"',"result":{"request_id":"'"$rid"'","op":"coding/references","data":[{"path":"src/main.rs","range":{"start":{"line":1,"character":4},"end":{"line":1,"character":7}}}]}}'
          ;;
        *'"op":"coding/hover"'*|*'\"op\":\"coding/hover\"'*)
          printf '%s\n' '{"jsonrpc":"2.0","id":'"$id"',"result":{"request_id":"'"$rid"'","op":"coding/hover","data":{"contents":"fn fixture()","range":{"start":{"line":10,"character":0},"end":{"line":10,"character":3}}}}}'
          ;;
        *'"op":"coding/diagnostics"'*|*'\"op\":\"coding/diagnostics\"'*)
          printf '%s\n' '{"jsonrpc":"2.0","id":'"$id"',"result":{"request_id":"'"$rid"'","op":"coding/diagnostics","data":[{"path":"src/lib.rs","range":{"start":{"line":2,"character":0},"end":{"line":2,"character":5}},"severity":"warning","message":"fixture diagnostic","code":"F001"}]}}'
          ;;
        *'"op":"coding/symbols"'*|*'\"op\":\"coding/symbols\"'*)
          printf '%s\n' '{"jsonrpc":"2.0","id":'"$id"',"result":{"request_id":"'"$rid"'","op":"coding/symbols","data":[{"name":"fixture","kind":"function","location":{"path":"src/lib.rs","range":{"start":{"line":40,"character":0},"end":{"line":50,"character":1}}}}]}}'
          ;;
        *'"op":"coding/cancel"'*|*'\"op\":\"coding/cancel\"'*)
          printf '%s\n' '{"jsonrpc":"2.0","id":'"$id"',"result":{"request_id":"'"$rid"'","op":"coding/cancel","data":{}}}'
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
