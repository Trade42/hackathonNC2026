#!/bin/sh
# Signs the iOS simulator app in as a demo persona (mock mode) by writing the
# session token into the WebView's localStorage, then relaunches it.
# Usage: e2e/ios-session.sh judge [ferry://path]
set -e
SIM=${SIM:-booted}
P=${1:-judge}
xcrun simctl terminate "$SIM" app.ferry.hacknc 2>/dev/null || true
D=$(xcrun simctl get_app_container "$SIM" app.ferry.hacknc data)
DB=$(find "$D/Library/WebKit" -name localstorage.sqlite3 | head -1)
HEX=$(printf "mock.usr_%s" "$P" | iconv -f UTF-8 -t UTF-16LE | xxd -p | tr -d '\n')
sqlite3 "$DB" "insert into ItemTable (key, value) values ('ferry.token', X'$HEX');"
xcrun simctl launch "$SIM" app.ferry.hacknc >/dev/null
