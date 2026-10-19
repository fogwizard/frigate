# Vigil 微信小程序示例（HTTP-FLV 预览）

对接 Vigil：

- `GET /api/streams` — 相机列表  
- `GET /api/streams/{camera}/play` — 短时签名 FLV 地址  

## 前置

1. Vigil `stream.enabled: true`，`public_base` 为已备案 HTTPS  
2. MediaMTX + Nginx（见 `packaging/`）  
3. 微信公众平台配置 request / 合法域名：Vigil API 域名、流媒体域名  
4. 真机才能稳定测直播；开发者工具可先看 `play` 接口是否返回 url  

## 使用

1. 用微信开发者工具打开本目录  
2. 改 `app.js` 里 `apiBase`、`apiToken`  
3. 编译预览  

`live-player` 需要 **HTTP-FLV**。若 MediaMTX 当前只出 HLS，需把出口配成 FLV 或换能出 FLV 的网关。
