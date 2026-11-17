#!/bin/bash
# Carry the FRP control connection over SSH. The stream server's cloud firewall
# drops ports other than the ones already published, so frps stays on loopback.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ssh -N \
  -o ExitOnForwardFailure=yes \
  -o ServerAliveInterval=30 \
  -o ServerAliveCountMax=3 \
  -o BatchMode=yes \
  -L 127.0.0.1:17000:127.0.0.1:7000 \
  ubuntu@122.51.106.239 &
tunnel=$!
cleanup() { kill "$tunnel" 2>/dev/null || true; }
trap cleanup EXIT

for _ in $(seq 1 50); do
  if nc -z 127.0.0.1 17000; then
    break
  fi
  if ! kill -0 "$tunnel" 2>/dev/null; then
    exit 1
  fi
  sleep 0.2
done

# Do not exec: bash EXIT traps run before exec and would kill $tunnel.
# Keep bash as the LaunchAgent process so the trap still cleans SSH when frpc exits.
/opt/homebrew/bin/frpc -c "$ROOT/config/frpc.toml"
status=$?
exit "$status"
