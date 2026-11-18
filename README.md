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

`config/default.yaml` 的 `stream.public_base` 与 `examples/miniprogram/app.js` 的 `apiBase` 必须是同一个地址，应用侧暂时仍是 `http://122.51.106.239`。Caddy 已加载 TrustAsia 证书（`/etc/caddy/certs`），本机 `https://rujibao.cn` 正常；公网访问该 Host 若被跳到 DNSPod `webblock`，是腾讯云未备案拦截，备案通过后再把 `public_base` / `apiBase` 改成 `https://rujibao.cn`。

公网机器上的 Caddy 听 80 端口，按路径分流：

- `/cam1.m3u8`、`/cam1.flv`、`/hls/*` 转给本机 go2rtc（`127.0.0.1:1984`）。小程序 `<video>` 播 Vigil `play` 返回的钉住会话地址 `/hls/playlist.m3u8?id=…`（不要每次打 master `/{camera}.m3u8`，否则 go2rtc 新会话卡住首秒，20 秒超时）。Caddy 会去掉 FLV/HLS 的 `Range`。HTTPS 已启用；证书在服务器 `/etc/caddy/certs/`，Caddyfile 用 `tls cert key` 加载。
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

## 排查记录（2026-11-17 晚）

关机再开后版本从 `0.1.39` 推进到 `0.1.46`。当晚踩过的坑和结论如下，方便以后对照。

### 登录 502：SSH 隧道被脚本自己杀掉

现象：小程序 login 很快返回 502；Caddy 正常，Vigil 本机 `8970` 也正常。

原因：`packaging/frpc-tunnel.sh` 里先起 `ssh -L 17000→7000`，再 `exec frpc`。bash 在 `exec` 前会跑 `EXIT` trap，把刚拉起的 SSH 杀掉；frpc 只剩自己，日志反复 `dial tcp 127.0.0.1:17000: connect: connection refused`。

处理：不要对 frpc 使用 `exec`，让 bash 一直当 LaunchAgent 主进程，退出时再清 SSH。改完后 `launchctl kickstart -k gui/$(id -u)/cn.rujibao.frpc`。服务器 `/tmp` 里的 `frp.tgz` 等安装包要删掉，腾讯云会当危险文件告警。

### 喂食器串口：macOS 不叫 ttyUSB0

现象：health 里 `feed_serial=open_failed`，本机没有 `/dev/ttyUSB0`。

结论：适配器已在，节点是 `/dev/cu.usbserial-110`（以及对应的 `tty.*`）。程序应开 `cu.*`。`config/default.yaml` 的 `feed.port` 已从 Windows 的 `COM5` 改成该路径。

手动喂食时先看 screen 里 Vigil 日志：

- 有 `miniprogram device command accepted … kind=feed` 说明请求到了。
- 若紧跟 `写入失败 … error=modbus response timeout`，说明串口已打开并写了帧，但从站（`feed_slave`）在 `timeout_ms` 内没回包——查接线、供电、从站地址和波特率，不是小程序或隧道问题。

### 小程序画面：超时、转圈、立刻失败

当晚试过三条路，最终稳定出画的是第三条。

1. **`<video>` + 公网 master `/{camera}.m3u8`**：能拉到流，但 go2rtc 每次 master 都新开 HLS 会话，只卡在前一两秒，小程序 20 秒 `video.timed_out`。
2. **`<live-player>` + `/{camera}.flv`**：开发者工具官方不支持；真机 ExoPlayer 会带 `Range: bytes=0-1023` 探测，go2rtc 直播 FLV 无视 Range 从头重推，连接被掐。堆栈若还停在旧的 `live.js:227`，说明微信里仍是旧包，必须重新编译。
3. **现行（`0.1.46`）**：`<video>`；`play` 返回钉住的 `http://122.51.106.239/hls/playlist.m3u8?id=…`（Vigil 按镜头缓存 go2rtc 会话）；Caddy 对 FLV 和 `/hls/*` 去掉 `Range`。不要再让手机去打 master `/{camera}.m3u8`。

对照日志：

- play 200 后约 100ms 就 `player_failed`、且服务器没有任何 `/hls` 或 `/cam*.flv`：多半是旧包或开发者工具里的 `live-player`。
- play 200 后整 20 秒 `video.timed_out`，Caddy 里有 `segment.ts` 但 `connection reset`：以前是会话/Range 问题；现在应检查 play 返回的是否已是 `/hls/playlist.m3u8?id=`。
- 证书已在 Caddy；公网 `Host: rujibao.cn` 若 TLS 被重置或跳到 DNSPod webblock，先完成 ICP 备案并在小程序后台配置 `https://rujibao.cn`，再改 `public_base` / `apiBase` 并重新编译。

### 重启 Vigil 注意

Mac 上 `vigil-infer` 冷启动会编 CoreML，可能超过主进程等待时间，出现 `unable to connect to infer at 127.0.0.1:9870`。再执行一次 `./start.sh` 通常即可（模型缓存热了会快很多）。只 SIGTERM 父进程是 screen 里那只 bash 拉起的主 `vigil`，不要杀 ffmpeg/infer 下面的 kqueue 副本。

## 仓库结构

```
crates/vigil-common   共享类型与 IPC
crates/vigil          主守护进程 + 运维子命令 (status/health/…)
crates/vigil-infer    推理侧车
config/               默认 YAML
web/                  静态 UI
packaging/            tar.gz / systemd / install.sh
```
