#!/usr/bin/env bash
# Builds and serves this example, in the foreground so Playwright owns it.
set -euo pipefail
cd "$(dirname "$0")"
RUN=".run-test"
cargo run --release --quiet -p nested-cards -- build --out dist --partial-out dist-partial >/dev/null
mkdir -p "$RUN"
cargo run --release --quiet -p nested-cards -- nginx --prefix "$RUN" --port 8097 \
  --out dist --partial-out dist-partial > "$RUN/nginx.conf"
exec nginx -p "$PWD/$RUN" -c "$PWD/$RUN/nginx.conf"
