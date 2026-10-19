# Vigil 微信小程序示例（HTTP-FLV 预览）

**当前尚未联调成功。** 自动化测试使用模拟响应，不代表真实登录、支付、视频或设备执行成功。
部署、运行、配置修改及分阶段排查请先阅读 [部署与调试手顺](DEBUGGING.md)。

对接 Vigil：

- `GET {boardingPath}/records` — 当前用户的寄养记录
- `GET /api/streams/{camera}/play` — 短时签名 FLV 地址  

## 前置

1. Vigil `stream.enabled: true`，`public_base` 为已备案 HTTPS  
2. MediaMTX + Nginx（见 `packaging/`）  
3. 微信公众平台配置 request / 合法域名：Vigil API 域名、流媒体域名  
4. 真机才能稳定测直播；开发者工具可先看 `play` 接口是否返回 url  

## 使用

1. 用微信开发者工具打开本目录  
2. 改 `app.js` 里 `apiBase`、`loginPath`，使用真实小程序 AppID；实现下文的后端登录接口
3. 编译预览  

`live-player` 需要 **HTTP-FLV**。若 MediaMTX 当前只出 HLS，需把出口配成 FLV 或换能出 FLV 的网关。

## 微信身份与登录

首页使用微信登录获取身份并显示后端返回的 OpenID，不再手工输入微信号。
`app.login()` 调用 `wx.login()`，将临时 `code` 发送给 `loginPath` 指定的后端接口。
后端用 AppID 和 AppSecret 调用微信 `code2Session`，获得 OpenID 并建立用户会话，返回：

```json
{"openid":"微信返回的用户标识","token":"后端签发的用户会话令牌"}
```

AppSecret、微信 session_key 只留在后端，不得返回给小程序。
用户身份和令牌只存于本次小程序运行内存；设备请求携带 `Authorization: Bearer <token>`。
寄养记录、播放、喂食、喂水和开门接口都必须在后端校验用户权限，不信任前端传入的用户标识。
接口返回 401 后清空登录身份，不自动重放设备操作。
当前目录只有客户端示例，登录、用户设备绑定和门禁控制后端尚未提供，不能直接连接未改造鉴权的 Vigil API。

直播页中间显示视频，底部为手动喂食和手动喂水按钮；播放失败时可点击“刷新画面”重新获取签名地址。

## 多寄养镜头选择

首页和直播页均使用号码下拉框，最多显示 64 个不重复的授权镜头。
普通用户的镜头入口只来自当前用户已支付且有效的寄养记录，不再使用普通流列表放行；管理员例外见下文。
`GET {boardingPath}/records` 响应格式：

```json
{"records":[
  {"id":"record_1","status":"active","paymentStatus":"paid","camera":"room_03","number":"03","label":"寄养房 A"},
  {"id":"record_2","status":"active","paymentStatus":"paid","camera":"room_18","number":"18","label":"寄养房 B"}
]}
```

`camera` 是接口使用的唯一镜头标识，`number` 是门店分配的寄养号码，`label` 是展示名称。
记录必须包含非空字符串 `id`、`camera`，且 `status=active`、`paymentStatus=paid` 才会展示。
不接受字符串相机列表作为寄养凭证；如未返回号码，前端仅按列表顺序临时编号，不代表固定笼位号。
超出 64 个镜头的部分不展示；后端也应按产品规则限制授权数量。
首页刷新会尽量保留已选镜头；直播页进入时重新加载授权列表，不直接信任页面参数。
如果传入镜头不在列表中，选择列表中的第一个授权镜头；空列表则不请求视频。
快速切换时忽略旧播放请求的响应；喂食、喂水确认和提交期间禁止切换镜头。
设备操作超时或失败同样冷却 10 秒，不自动重试。冷却只属于当前页面，不能替代服务端的频率限制和操作去重。
播放或设备操作返回 401/403 时清空当前直播访问状态，点击刷新可重新登录或加载授权列表。
服务端仍必须逐次验证播放、喂食和喂水权限；前端列表校验不能实现真正的用户隔离。

## 管理员查看全部镜头

管理员不需要选笼位、付款或创建寄养记录即可查看镜头。后端登录接口需根据数据库中当前 OpenID 的角色返回：

```json
{"openid":"管理员的微信标识","token":"管理员会话令牌","role":"admin"}
```

仅精确的 `role: "admin"` 启用此浏览分支；不返回 role 或其他值均按普通用户处理。
角色由后端决定，不在小程序中设置管理员名单，也不能通过手改 `app.js` 真正获得权限。
角色仅保存在当前登录会话内存中，重新登录时刷新；401 时清空身份并重置角色。

管理员的首页和直播页读取 `GET /api/streams`，不查询寄养记录，因此无需配置 `boardingPath` 就能浏览。
服务端必须从会话令牌识别管理员并返回管理范围内全部镜头，不得因客户端请求该路径就放行。
管理员页面显示“管理员 · 全部镜头”，进入视频仍逐次请求签名播放地址，默认打开第一个镜头，可用下拉框切换。
沿用默认最多 64 个镜头的展示限制，不会要求先去寄养页面选笼位。
普通用户只请求寄养记录，即使没有记录，也不会回退到全部镜头接口。
管理员的列表或播放请求失败时同样不绕过后端授权；管理员无镜头时显示配置提示，不要求扫码寄养。

管理员镜头列表响应示例：

```json
{"cameras":[
	{"camera":"room_01","number":"01","label":"寄养房 A","canControl":false},
	{"camera":"room_02","number":"02","label":"寄养房 B","canControl":true}
]}
```

也兼容 `{"cameras":["room_01","room_02"]}`，此时管理员默认仅可观看。
管理员按当前镜头的 `canControl` 决定控制权限：后端明确返回布尔值 `true` 时，显示喂食、喂水入口；`false`、未返回或字符串 `"true"` 时只可观看。
页面随镜头切换更新控制权限，不把一个镜头的权限带到其他镜头。管理员每次喂食、喂水请求前重新查询镜头列表，确认目标镜头仍有 `canControl: true` 才发送设备指令。
启用控制还需在 `app.js` 配置真实的 `feedPath` 和 `waterPath`；无需为管理员创建寄养记录。
普通用户仍按有效寄养记录使用喂食和喂水功能，不受管理员条件控制规则影响。
服务端必须对每次喂食、喂水请求独立检查当前用户对目标镜头的控制权限，无权限时返回 403；不信任客户端字段或列表检查，前端限制不能代替服务端权限校验。
管理员播放接口应允许有对应查看权限的管理员在无寄养记录时访问；普通用户继续要求有效且已支付的寄养记录。
撤销管理员角色后，服务端应立即失效原会话或在每次请求重新验证角色，不应等待客户端刷新才撤销权限。

## 扫码寄养与支付

首页增加“扫一扫寄养”，普通用户无有效记录时提示扫码寄养，不允许进入视频；管理员查看视频不受此限制。
寄养页面流程：微信登录 -> 扫描门店寄养二维码 -> 选择可用笼位 -> 确认寄养 -> 后端创建订单 -> 确认订单最终金额 -> 微信支付 -> 查询服务端支付及寄养状态 -> 进入对应镜头。
已有有效记录的用户可继续扫码新增其他寄养，镜头下拉框展示最多 64 个不同笼位镜头。

在 `app.js` 配置：

```javascript
boardingPath: "/api/boarding",
```

默认为空，未配置会提示“尚未配置有效的寄养接口”，不会回退到无寄养记录的镜头访问模式。
该目录仅实现客户端，没有支付商户服务、微信支付回调、库存数据库或真实订单后端；配置路径本身不会开通支付。
必须使用真实小程序 AppID、已关联的小程序支付商户号，并在真机上测试 `wx.requestPayment`。

门店寄养二维码与开门二维码不同，内容示例：

```json
{"type":"petshop-boarding","version":1,"token":"boarding_token_123456"}
```

token 为服务端签发的门店凭证，格式为 16 至 512 个字母、数字、下划线或连字符；服务端需校验真实性、有效期及门店绑定。
所有接口都携带 `Authorization: Bearer <用户会话令牌>`，服务端从令牌获取 OpenID，不接受客户端指定订单所有者。

### 笼位查询

`POST {boardingPath}/cages` 请求 `{"qrToken":"二维码凭证"}`，响应：

```json
{"storeName":"宠物店 A","cages":[
	{"id":"cage_03","number":"03","amount":12000,"available":true}
]}
```

`id`、`number` 必须为非空字符串；`amount` 是本次寄养总费用，单位为人民币分，必须是正整数。
当前客户端未提供天数、入住日期等计价输入，服务端必须明确一次订单的寄养时长、开始和结束规则，并可通过门店说明展示。
仅可用笼位可选，最多展示 64 个；可用状态只是报价快照，不等于已占用。

### 创建订单

`POST {boardingPath}/orders` 请求：

```json
{"qrToken":"二维码凭证","cageId":"cage_03","requestId":"本次下单的唯一标识"}
```

服务端重新计算价格，原子预留笼位，并使用 `(用户, requestId)` 做持久化幂等：相同请求重复提交必须返回同一订单，不得重复占用或收费。
同一标识更改笼位或门店应拒绝；过期、已支付或已关闭订单不能重新创建成另一个订单。
客户端不会提交价格，也不会自动生成新下单标识重试未知结果；手动查询恢复时可能使用原标识重发创建请求。
服务端还需防止同一用户跨设备并发下单重复占用，不能仅依赖本地存储保护。

响应格式：

```json
{
	"orderId":"order_1","status":"pending","paymentStatus":"pending","amount":12000,
	"payment":{
		"timeStamp":"1234567890","nonceStr":"服务端生成",
		"package":"prepay_id=微信预支付标识","signType":"RSA","paySign":"服务端签名"
	}
}
```

订单最终金额会再次展示确认。支付参数需由后端通过微信支付下单接口生成并签名，不得把商户私钥放入小程序。

### 查询与发放权限

`GET {boardingPath}/orders/{orderId}` 必须校验订单属于当前用户；支付完成且寄养生效时响应：

```json
{"orderId":"order_1","status":"active","paymentStatus":"paid","recordId":"record_1","camera":"room_03"}
```

只有该状态并且记录接口同时返回对应有效镜头时，客户端才跳转视频；直播页还会重新查询记录。
未支付时返回 `status=pending,paymentStatus=pending`；允许继续支付时还应返回金额和有效 payment 参数。
已支付待生效时返回 `paymentStatus=paid`，status 不得提前设为 active；客户端保留订单并提示稍后查询。
`status=closed/cancelled/refunded` 表示当前订单已终止，客户端清理待确认订单，允许重新扫码。
不要使用 404 代替订单状态；未知结果会保留订单，需要后台查询或联系门店处理。

后端必须验证微信支付通知签名、商户号、AppID、订单号、币种和实际支付金额，并通过可信支付结果原子生成寄养记录、绑定笼位镜头、开放喂食喂水权限。
客户端支付回调、传入的 requestId 或本地存储都不能作为支付成功的证据；支付取消、失败或超时不会自动再次付款。
待确认订单按 OpenID 保存在本地，重新打开寄养页面可查询或继续支付同一订单；本地丢失时后端仍必须有订单查询和幂等保护。
退款、退养、寄养到期时，后端撤销视频和设备操作权限，并释放笼位；每次播放、喂食、喂水请求都要验证有效寄养记录。

## 扫一扫开门

首页“门店通行”区域新增“扫一扫开门”。在 `app.js` 中配置真实接口路径即可启用：

```javascript
apiBase: "https://你的后端域名",
loginPath: "/api/miniprogram/login",
doorPath: "/api/doors/open",
```

路径仅为建议，后端需自行实现；`doorPath` 默认为空时按钮禁用。
流程为：用户确认使用微信身份 -> 微信登录 -> 相机扫描门店二维码 -> 再次确认开门 -> 后端校验并控制门锁。
`wx.login` 确认的是微信用户身份，不等于获得门禁权限，也不需要获取昵称、头像或手机号。
页面中的身份确认弹窗是本小程序的操作确认，不是微信平台的授权弹窗；相机访问权限由微信系统处理。

二维码采用如下 JSON 格式，`token` 是后端生成的随机、短时有效的开门凭证，不是用户登录令牌：

```json
{"type":"petshop-door","version":1,"token":"door_challenge_123456789"}
```

上面的 token 仅展示格式，不能用于真实开门。实际 token 必须为 16 至 512 个字母、数字、下划线或连字符。
客户端只接受该格式，并且仅从相机扫描，不从相册选择；普通网址或其他二维码不会触发设备请求。
二维码内容不会作为请求 URL，也不会在客户端作为开门授权依据。

开门请求：

```http
POST /api/doors/open
Authorization: Bearer <用户会话令牌>
Content-Type: application/json

{"qrToken":"二维码中的短时凭证"}
```

后端职责：

- 从会话令牌识别用户，验证用户对该门店的有效通行权限；微信登录成功不自动赋予通行权限。
- 验证 qrToken 的真实性、有效期、对应门店和大门，原子消费单次凭证，防止复制或重放。
- 使用服务端保存的门禁凭据调用设备，不向客户端暴露门锁密钥；记录操作审计并限制频率。
- 401 表示会话失效，403 表示无门店权限；凭证过期或重复使用应拒绝执行，不能绕过权限验证。
- 仅设备确认开锁后才返回 `{"success":true,"status":"opened"}`。
- 如仅收到提交回执，返回 `{"success":true,"status":"accepted"}`；页面只显示申请受理，不声称门已打开。

请求期间按钮锁定，发出申请后冷却 10 秒；失败或超时不会自动重试，因为门锁可能已收到指令。
前端冷却不能替代后端的去重、权限验证和限流。二维码签发与门锁硬件接入尚需后端实现。
真实微信登录、相机权限和门锁响应需在微信真机环境验证。

## 接入手动喂食

当前示例没有提供后端喂食服务，`app.js` 中的 `feedPath` 默认为空，按钮因此禁用。
后端实现设备控制后，将其设置为相对 API 路径，例如 `/api/feeding`。此路径仅为示例，不代表 Vigil 已提供该接口。

接口约定：

- 向 `${apiBase}${feedPath}` 发送 `POST`，JSON 请求体为 `{ "camera": "相机名称" }`。
- 使用 `Authorization: Bearer <用户会话令牌>`，后端必须校验身份及对应设备的操作权限。
- HTTP 2xx 表示指令已接受；如业务拒绝，应返回非 2xx 或 `{ "success": false, "error": "原因" }`。
- 后端负责将相机映射到实际喂食设备，限制投喂量、执行频率并防止重复执行。不能仅依靠前端保护。

每次操作需要确认，请求期间禁止重复点击；提交成功后按钮冷却 10 秒。
页面仅提示“指令已提交”，不将接口成功等同于设备完成投喂。
请求失败或超时不自动重试，因为设备可能已经收到指令，需要先核实设备状态。

## 接入手动喂水

`app.js` 中新增 `waterPath`，默认为空，未配置时喂水按钮禁用，不影响喂食功能。
后端实现喂水控制后，填入相对 API 路径，例如 `/api/watering`。该路径仅为示例，并非已存在的 Vigil 接口。

- 向 `${apiBase}${waterPath}` 发送 `POST`，JSON 请求体同喂食接口：`{ "camera": "相机名称" }`。
- 复用用户会话鉴权和喂食接口的响应约定；后端必须校验对应设备的操作权限。
- 手动喂水同样需要确认，提交期间防止重复点击，成功后独立冷却 10 秒；超时或失败不自动重试。
- 页面只确认指令已提交，不代表设备已完成出水。后端需限制单次出水量、执行频率并防止重复执行。

## 验证

在 `examples` 目录执行 `node --test --test-reporter=dot ./miniprogram/tests/logger.test.js ./miniprogram/tests/admin.test.js ./miniprogram/tests/boarding-page.test.js ./miniprogram/tests/boarding.test.js ./miniprogram/tests/cameras.test.js ./miniprogram/tests/feeding.test.js ./miniprogram/tests/door.test.js`，验证日志脱敏、管理员例外、寄养支付、多镜头选择、登录鉴权、喂食喂水、二维码解析、开门确认、权限拒绝与异常保护。
视频播放、原生组件布局与真实设备喂食、喂水仍需使用微信开发者工具及真机联调；普通浏览器无法运行该小程序。

## 日志与排查

日志统一通过 `utils/logger.js` 输出到微信开发者工具 Console，真机可通过真机调试查看。
按 `[miniprogram]` 或事件名筛选；info 记录流程关键节点，warn 记录拒绝、取消后的不确定状态或等待生效，error 记录网络、登录和播放器故障。
不会对每一帧播放或每次缓存命中打印日志，播放器仅在播放、缓冲状态变化及异常时输出。

关键事件：

- `login.started/completed/failed`：微信登录与后端会话交换；completed 中的 isAdmin 表示当前角色。
- `request.started/completed/network_failed/blocked`：请求生命周期；同一请求使用相同 requestId，operation 是稳定操作名，elapsedMs 是耗时，statusCode 是 HTTP 状态。
- `session.expired`：401 导致用户会话及角色已清空，需要重新登录。
- `cameras.loaded`、`video.camera_selected`：镜头列表数量、身份与当前镜头控制权限，不输出镜头名称或标识。
- `video.url_missing/playing/buffering/interrupted/player_failed`：签名地址缺失、播放与播放器错误码，不输出原始播放器对象。
- `device.blocked/permission_denied/rejected/submitted/result_uncertain`：前端阻止、后端拒绝、指令受理或结果未知。
- `boarding.pending_restored/order_created/cages_loaded`：待确认订单恢复、创建与可用笼位数。
- `payment.started/client_completed/flow_incomplete/query_failed`：支付调用与结果查询；client_completed 的 outcome 为 pending，不代表后端确认收款。
- `boarding.permissions_pending/activation_pending/activated/order_terminated`：支付后的寄养授权进度。
- `door.flow_started/scan_cancelled/confirm_cancelled/flow_failed/result`：扫码开门各阶段；accepted 与 opened 含义不同。

requestId 仅为客户端本次运行内的日志关联编号，不是支付幂等 requestId，也未作为服务端链路追踪头发送。
日志只允许预定义字段和枚举值，不记录请求体、响应体、Header、完整路径、OpenID、登录 code、会话令牌、二维码凭证、订单标识、支付签名或签名视频地址，也不直接打印原始异常消息。
不要在排查时改为打印整个接口返回、订单或播放器事件对象。

建议按请求 operation 和 requestId 查找错误：401 检查登录，403 检查目标设备权限，network_failed 检查网络和合法域名；支付出现 client_completed 后持续 activation_pending 时需检查后端微信通知处理和寄养记录生成。
device.submitted 或 door.result=accepted 仅表示服务端受理，不代表设备已实际执行，需结合服务端与设备日志核实。
