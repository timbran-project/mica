#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

fileins=(
  apps/shared/sync-host.mica
  apps/shared/buffers.mica
)
for name in schema windows buffers keymaps undo commands session picker minibuffer files ui defaults http host-policy; do
  fileins+=("apps/editor/$name.mica")
done
args=()
for filein in "${fileins[@]}"; do
  args+=(--filein "$filein")
done

exec cargo run --bin mica-daemon -- "${args[@]}" \
  --web-bind "${MICA_EDITOR_BIND:-127.0.0.1:8008}" "$@"
