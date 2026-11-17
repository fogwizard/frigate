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

## 手机离网登录

小程序不在局域网时到不了 Mac 的 `192.168.1.13:8970`。登录、寄养记录、播放地址、喂食和喂水改走公网；录像、网页和文件仍只留在这台 Mac 上。

`config/default.yaml` 的 `stream.public_base` 与 `examples/miniprogram/app.js` 的 `apiBase` 必须是同一个地址，现在是 `http://122.51.106.239`。域名证书就绪后两处一起改成 `https://rujibao.cn`。

公网机器上的 Caddy 听 80 端口，按路径分流：

- `/cam1.m3u8`、`/cam1.flv`、`/hls/*` 转给本机 go2rtc（`127.0.0.1:1984`）。小程序 `<video>` 播 `.m3u8`（不依赖 live-player 类目权限；开发者工具也不支持 live-player）。Caddy 仍会去掉 FLV 的 `Range`，留给以后的 live-player。`rujibao.cn` 已解析到这台机器，正式环境还要开公网 443 并改成 HTTPS。
- `/api/miniprogram/*`、`/api/boarding/records`、`/api/streams`、`/api/streams/*/play`、`/api/feeding`、`/api/watering` 转给 `127.0.0.1:18970`。
- 其他路径直接回 404，不会进隧道。

`18970` 不是 Vigil 直接开在公网上的端口。frps 只绑在服务器的 `127.0.0.1:7000`，并把隧道端口绑在 `127.0.0.1:18970`。云防火墙会丢掉新的公网端口，所以 Mac 先用 SSH 把本机 `127.0.0.1:17000` 转到服务器的 `127.0.0.1:7000`，frpc 再连这个本机端口，把 Mac 的 `127.0.0.1:8970` 送过去。Vigil 仍由 `web.bind` 监听 `0.0.0.0:8970`，只给局域网用，不要在路由器上做端口映射。

配置和进程：

- 服务器：`/etc/frp/frps.toml`、`frps.service`，Caddy 配置来自 `packaging/Caddyfile`。
- Mac：`config/frpc.toml`（不进 git）、`packaging/frpc-tunnel.sh`，由 `~/Library/LaunchAgents/cn.rujibao.frpc.plist` 拉起。日志在 `data/frpc.log`。

排查时先看隧道，再看公网路径：

```bash
pgrep -lf 'frpc|ssh -N'
tail -n 20 data/frpc.log
curl -sS -D - -o /dev/null -X POST http://122.51.106.239/api/miniprogram/login \
  -H 'content-type: application/json' -d '{}'
curl -sS -o /dev/null -w '%{http_code}\n' http://122.51.106.239/api/recordings
```

登录接口应回到 Vigil（缺 `code` 时是 422）。`/api/recordings` 应是 404，且不会进入隧道。frpc 日志里有 `start proxy success` 才说明隧道已建立。登录一直 `502`、且 `data/frpc.log` 出现 `dial tcp 127.0.0.1:17000: connect: connection refused`，说明 SSH 本地转发没起来：`pgrep -lf 'ssh -N'` 应有进程，`nc -z 127.0.0.1 17000` 应成功；没有就 `launchctl kickstart -k gui/$(id -u)/cn.rujibao.frpc`。小程序改过 `apiBase` 后要重新编译；页面停在「未登录」且服务器没有新的登录请求，说明点击没有离开手机。

## 仓库结构

```
crates/vigil-common   共享类型与 IPC
crates/vigil          主守护进程 + 运维子命令 (status/health/…)
crates/vigil-infer    推理侧车
config/               默认 YAML
web/                  静态 UI
packaging/            tar.gz / systemd / install.sh
```
