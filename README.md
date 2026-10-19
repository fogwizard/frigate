# Vigil

精简版 Frigate 替代方案：Rust + OpenVINO，面向 Ubuntu x86-64。

- 人员 / 猫 / 狗检测（可配置）
- 5 路以上 RTSP 连续分段录像
- 事件快照与短视频、Webhook
- 内置 Web UI（实时 / 事件 / 录像）
- 单包交付：`vigil-linux-x86_64.tar.gz`（含多二进制）

## 快速开始

```bash
cargo build --release
# 另开终端
./target/release/vigil-infer --endpoint 127.0.0.1:9870
./target/release/vigil --config config/default.yaml --web-dir web
# 运维（同一二进制）
./target/release/vigil status
./target/release/vigil health
```

浏览器打开 http://127.0.0.1:8970 。

详细开发与模型导出见 [docs/DEV.md](docs/DEV.md)。打包见 `packaging/build-bundle.sh`。

## 仓库结构

```
crates/vigil-common   共享类型与 IPC
crates/vigil          主守护进程 + 运维子命令 (status/health/…)
crates/vigil-infer    推理侧车
config/               默认 YAML
web/                  静态 UI
packaging/            tar.gz / systemd / install.sh
```
