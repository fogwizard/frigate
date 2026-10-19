#!/usr/bin/env bash
# Install Vigil from extracted vigil-linux-x86_64/ directory.
set -euo pipefail

PREFIX="${PREFIX:-/opt/vigil}"
CONFIG_DIR="${CONFIG_DIR:-/etc/vigil}"
DATA_DIR="${DATA_DIR:-/var/lib/vigil}"
SERVICE_USER="${SERVICE_USER:-vigil}"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

if [[ "$(id -u)" -ne 0 ]]; then
  echo "Please run as root (sudo ./install.sh)"
  exit 1
fi

echo "==> installing to $PREFIX"
mkdir -p "$PREFIX" "$CONFIG_DIR" "$DATA_DIR" /run/vigil

id -u "$SERVICE_USER" >/dev/null 2>&1 || useradd --system --home "$DATA_DIR" --shell /usr/sbin/nologin "$SERVICE_USER"

rsync -a --delete \
  --exclude install.sh \
  "$SCRIPT_DIR/bin" "$SCRIPT_DIR/lib" "$SCRIPT_DIR/models" "$SCRIPT_DIR/share" \
  "$PREFIX/" 2>/dev/null || {
  cp -a "$SCRIPT_DIR/bin" "$SCRIPT_DIR/lib" "$SCRIPT_DIR/models" "$SCRIPT_DIR/share" "$PREFIX/"
}

if [[ ! -f "$CONFIG_DIR/config.yaml" ]]; then
  cp "$SCRIPT_DIR/config/default.yaml" "$CONFIG_DIR/config.yaml"
  # Point paths at data dir
  sed -i "s|data_dir: data|data_dir: $DATA_DIR|" "$CONFIG_DIR/config.yaml"
  sed -i "s|db_path: data/vigil.db|db_path: $DATA_DIR/vigil.db|" "$CONFIG_DIR/config.yaml"
  sed -i "s|model: models/|model: $PREFIX/models/|" "$CONFIG_DIR/config.yaml"
fi

chown -R "$SERVICE_USER:$SERVICE_USER" "$DATA_DIR" /run/vigil
chmod 755 "$PREFIX/bin/"*

UNIT_SRC="$SCRIPT_DIR/systemd/vigil.service"
UNIT_DST="/etc/systemd/system/vigil.service"
sed -e "s|@PREFIX@|$PREFIX|g" \
    -e "s|@CONFIG@|$CONFIG_DIR/config.yaml|g" \
    -e "s|@DATA@|$DATA_DIR|g" \
    -e "s|@USER@|$SERVICE_USER|g" \
    "$UNIT_SRC" > "$UNIT_DST"

# GPU / OpenCL hint
if ! ls /dev/dri/render* >/dev/null 2>&1; then
  echo "NOTE: no /dev/dri/render* — OpenVINO GPU may fall back to CPU"
fi

systemctl daemon-reload
systemctl enable vigil.service
echo "==> installed. Edit $CONFIG_DIR/config.yaml then: systemctl start vigil"
echo "    UI: http://<host>:8970"
