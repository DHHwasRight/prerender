#!/usr/bin/env bash
# Records the README's tour and encodes it as an animated WebP.
#
# Playwright records WebM only, so the conversion is a second step. ffmpeg-static
# is a devDependency so this needs no system ffmpeg; one on PATH is used if it is
# there.
set -euo pipefail
cd "$(dirname "$0")/.."

FFMPEG=$(node -p "require('ffmpeg-static')" 2>/dev/null || command -v ffmpeg || true)
if [ -z "$FFMPEG" ] || [ ! -x "$FFMPEG" ]; then
  echo "no ffmpeg: run 'npm install', or install ffmpeg" >&2
  exit 1
fi

npx playwright test -c playwright.record.config.ts

# 15fps is enough for a 700ms ring to read, and the content is flat colour, so
# the whole tour costs less than a screenshot of a real page would.
"$FFMPEG" -y -hide_banner -loglevel error \
  -i docs/tour.webm \
  -vf "fps=15,scale=1000:-1:flags=lanczos" \
  -c:v libwebp_anim -lossless 0 -q:v 85 -compression_level 6 -loop 0 \
  -an -fps_mode passthrough \
  docs/tour.webp

ls -lh docs/tour.webp
