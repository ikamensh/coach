#!/usr/bin/env bash
# Bump the app version. Source of truth: package.json (feeds UI via Vite).
# Usage: ./scripts/bump-version.sh        (auto-increment patch)
#        ./scripts/bump-version.sh 0.2.0  (set explicit version)
set -euo pipefail
cd "$(dirname "$0")/.."

CURRENT=$(node -p "require('./package.json').version")

if [[ -n "${1:-}" ]]; then
  NEW="$1"
else
  IFS='.' read -r major minor patch <<< "$CURRENT"
  NEW="$major.$minor.$((patch + 1))"
fi

echo "$CURRENT -> $NEW"

sed -i '' "s/\"version\": \"$CURRENT\"/\"version\": \"$NEW\"/" package.json
sed -i '' "s/\"version\": \"$CURRENT\"/\"version\": \"$NEW\"/" src-tauri/tauri.conf.json
sed -i '' "s/^version = \"$CURRENT\"/version = \"$NEW\"/" Cargo.toml

echo "Done: $NEW"
