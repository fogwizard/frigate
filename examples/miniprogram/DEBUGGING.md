# 部署、运行与联调手顺

## 1. 当前状态与边界

**本程序尚未联调成功。自动化测试使用模拟微信 API 和模拟响应，不能证明真实登录、微信支付、视频播放或设备执行成功。**

本目录是微信原生小程序客户端，不是完整可部署的宠物店业务系统。目前默认配置为：

- `app.js` 中 `apiBase` 为 `https://api.example.com`，不是实际服务地址。
- `project.config.json` 中 AppID 为 `touristappid`，不能用于真实业务登录与支付。
- `feedPath`、`waterPath`、`doorPath`、`boardingPath` 为空，对应服务尚未接入。
- 本目录没有业务后端启动脚本、数据库迁移、微信支付通知处理、设备驱动或流媒体部署脚本。
- 没有浏览器预览版或可直接启用的 Mock 开关；普通浏览器、Live Server 不能运行 WXML 页面。

因此，导入开发者工具可以看到界面，但仅修改接口路径不能完成联调。本文的接口路径是客户端约定，不代表已有 Vigil 服务提供这些接口。

详细请求和响应格式见 [README.md](README.md)。请先完成下面的依赖检查，再按顺序验证，不要一开始同时调支付、视频和硬件。

## 2. 部署对象与准备清单

| 对象 | 部署或配置位置 | 必须准备的内容 |
| --- | --- | --- |
| 小程序客户端 | 微信开发者工具、微信公众平台 | 正式 AppID、开发者权限、体验成员、所需组件资质 |
| 业务 API | 自有服务器或已选定的运行平台 | 登录、用户角色、门店、笼位、订单、寄养记录、权限校验 |
| 数据库 | 后端使用的数据库 | OpenID 用户映射、设备绑定、笼位库存、订单与操作审计 |
| 微信登录 | 后端调用微信接口 | 对应小程序的 AppID、AppSecret，服务端访问微信网络 |
| 微信支付 | 商户平台和后端 | 关联商户号、支付签名配置、微信通知验签与可信查单 |
| 视频链路 | 摄像头、流媒体服务器、HTTPS 出口 | 摄像头取流、适配 `live-player` 的输出、签名地址和访问控制 |
| 喂食、喂水、门禁 | 设备控制网关和业务后端 | 设备协议、凭据、镜头到设备映射、限流与结果回执 |

部署顺序：

1. 准备测试门店、一个镜头、一个笼位、普通测试账号和管理员账号。
2. 在后端工程中实现并部署 API、数据库及会话鉴权。具体构建和启动命令取决于后端工程，本客户端目录无法提供可执行的后端部署命令。
3. 配置有效 HTTPS 证书和 API 域名；反向代理必须透传 `Authorization`，不得把 API 错误统一改成 HTTP 200。
4. 配置后端 AppID、AppSecret、支付凭据及设备密钥。密钥只放在服务端，不能写入小程序代码或日志。
5. 独立验证摄像头到流媒体出口可用：先起 go2rtc（`packaging/go2rtc.yaml`），再 Vigil restream，用 curl/VLC 打本机 `http://127.0.0.1:1984/api/stream.flv?src=cam1` 确认是 FLV，再经 Nginx 的 `/cam1.flv` 给小程序。
6. 在微信公众平台配置业务 API 的 request 合法域名，并按视频组件要求配置流媒体域名。手机必须能访问 API 和视频出口，不能只在服务器本机可访问。
7. 按下一节修改小程序配置，在开发者工具编译，再进行真机联调。

不要暴露摄像头 RTSP 地址、门禁密钥或服务端共享管理令牌给客户端。这里的 `sessionToken` 是后端签发的用户会话令牌，不是微信 `access_token` 或 `session_key`。

## 3. 需要修改的配置

### 3.1 客户端业务配置

修改 [app.js](app.js) 的 `globalData`，以下仅为路径示例，必须与实际后端一致：

```javascript
apiBase: "https://你的业务API域名",
loginPath: "/api/miniprogram/login",
feedPath: "/api/feeding",
waterPath: "/api/watering",
doorPath: "/api/doors/open",
boardingPath: "/api/boarding",
```

| 配置项 | 规则和影响 |
| --- | --- |
| `apiBase` | API 根地址，不带末尾斜杠；不是视频流地址 |
| `loginPath` | POST 登录接口，只接收 `wx.login` 的临时 code |
| `boardingPath` | 寄养接口前缀，以 `/` 开头且不带末尾 `/`；普通用户加载记录必须配置 |
| `feedPath` | 为空时喂食按钮禁用，必须指向实际设备业务接口 |
| `waterPath` | 为空时喂水按钮禁用，与喂食配置独立 |
| `doorPath` | 为空时扫码开门按钮禁用 |
| `openid/sessionToken/role` | 运行时登录结果，不手工填写，不用于配置管理员 |

路径不能填写完整 URL 或以 `//` 开头。完整地址由 `apiBase + 路径` 拼接。
调试不同环境前先确认目标域名；本项目没有自动的开发、测试、生产环境切换，避免测试设备误连生产服务。

### 3.2 微信工程与页面配置

- [project.config.json](project.config.json)：将 `appid` 替换为真实小程序 AppID，或通过开发者工具项目设置修改并核对文件。保留 `compileType: "miniprogram"`。
- 当前 `libVersion` 为 `3.5.5`。首次排查先记录所用基础库版本；若更换版本，应重新做真机回归，不把版本调整当作所有问题的修复。
- [app.json](app.json)：已注册首页、直播页和寄养页，通常不需修改；出现页面不存在时先检查 pages 注册和路径大小写。
- 页面 JSON、WXML、WXSS 是界面和组件配置，不是后端鉴权配置。不要通过隐藏按钮或修改 `isAdmin` 来代替服务端授权。

### 3.3 后端和微信平台配置

后端配置文件不在当前目录。由后端工程负责人确认 AppID、AppSecret、用户会话、管理员角色、微信支付通知地址、笼位价格/时长、镜头映射、设备接口及数据库连接。
微信支付通知路径由后端定义，不是小程序的 `boardingPath`，微信支付服务器必须能访问它。
确认当前小程序主体、类目及权限允许使用 `live-player`；开发者工具能编译不等于账号具备真实播放权限。

## 4. 如何运行

### 4.1 微信开发者工具

1. 安装微信开发者工具并用具有开发权限的微信账号登录。
2. 导入项目，目录选择当前 `miniprogram` 文件夹，不选其上级 `examples`。
3. 选择真实 AppID，核对上述业务配置，点击“编译”。当前工程不需要 npm 安装或前端打包命令。
4. 打开调试器 Console 和 Network，清空旧日志，重新编译或重启小程序以取得一次新的登录流程。
5. 查看首页是否完成登录，再检查管理员镜头或普通用户寄养记录；不要在登录失败时继续调播放。
6. 使用“预览”或“真机调试”在手机上验证扫码、支付、视频和设备。

本地接口调试可临时在工具“详情 / 本地设置”关闭合法域名与 HTTPS 证书校验；仅限开发排查，不能作为正式部署方案。最终必须在打开校验的条件下验收。
手机上的 `localhost`、`127.0.0.1` 指向手机自身，不是开发电脑；工具可访问的本地地址不代表手机可以访问。真机测试优先使用可访问的 HTTPS 测试环境。

### 4.2 自动化测试

在工作区 `examples` 目录执行（需要支持 `node:test` 的 Node.js，建议受支持的 Node.js LTS）：

```powershell
node --test --test-reporter=dot ./miniprogram/tests/logger.test.js ./miniprogram/tests/admin.test.js ./miniprogram/tests/boarding-page.test.js ./miniprogram/tests/boarding.test.js ./miniprogram/tests/cameras.test.js ./miniprogram/tests/door.test.js ./miniprogram/tests/feeding.test.js
```

测试覆盖客户端状态、权限分支、请求格式、支付回调处理和日志脱敏。它不会调用真实微信登录、扣款、连接真实视频或操作设备，不能作为联调通过证据。

## 5. 关键代码与请求路径

| 代码位置 | 负责内容 |
| --- | --- |
| [app.js](app.js) | `login()`：微信 code 换用户会话，保存 OpenID 和角色 |
| [utils/api.js](utils/api.js) | 请求、用户令牌、401 清理、业务 API、管理员控制复核 |
| [utils/cameras.js](utils/cameras.js) | 规范化镜头列表、去重和最多 64 个镜头 |
| [pages/index/index.js](pages/index/index.js) | 首页记录/镜头加载、进入视频前复查、扫码开门 |
| [pages/live/live.js](pages/live/live.js) | 进入视频再查权限、切换镜头、播放状态、喂食和喂水 |
| [pages/boarding/boarding.js](pages/boarding/boarding.js) | 扫码、选笼位、订单恢复、支付、后端状态查询和跳转 |
| [utils/boarding.js](utils/boarding.js) | 寄养二维码、笼位和支付参数校验 |
| [utils/door.js](utils/door.js) | 开门二维码格式和相机扫码 |
| [utils/logger.js](utils/logger.js) | 白名单结构化日志与脱敏 |

下面假设 `boardingPath=/api/boarding`，其余采用上面的示例配置：

| 操作 / 日志 operation | HTTP 路径 | 请求要点 |
| --- | --- | --- |
| 登录 / `login` | POST `/api/miniprogram/login` | `{code}`，无用户 Bearer；返回非空字符串 openid、token，可返回 role |
| 管理员镜头 / `streams` | GET `/api/streams` | 返回 `{cameras:[...]}`；管理员不依赖寄养记录 |
| 用户寄养 / `records` | GET `/api/boarding/records` | 返回 `{records:[...]}`；仅 paid + active 的有效记录产生视频入口 |
| 查询笼位 / `cages` | POST `/api/boarding/cages` | `{qrToken}`；返回门店和可用笼位，金额单位为分 |
| 下单 / `create_order` | POST `/api/boarding/orders` | `{qrToken,cageId,requestId}`；后端重新计价和幂等预留 |
| 查单 / `order` | GET `/api/boarding/orders/{orderId}` | 查询当前用户订单，不能只凭支付客户端回调开权限 |
| 播放 / `play` | GET `/api/streams/{camera}/play` | 返回 `{url}`；路径中的 camera 经过编码 |
| 喂食 / `feed` | POST `/api/feeding` | `{camera}`，无自动重试 |
| 喂水 / `water` | POST `/api/watering` | `{camera}`，无自动重试 |
| 开门 / `door` | POST `/api/doors/open` | `{qrToken}`；区分 accepted 与 opened |

除登录外，所有请求使用 `Authorization: Bearer <用户会话令牌>`，请求超时为 15 秒。不能直接以旧的共享 `X-Vigil-Token` 替代用户鉴权。
`/api/streams` 与 `/api/streams/{camera}/play` 当前是固定代码路径，不由 `app.js` 单独配置；后端不同路径需在接口适配层或 [utils/api.js](utils/api.js) 调整并同步测试。

## 6. 分阶段联调与通过条件

### 阶段 A：先打通登录

1. 配置真实 AppID、API 域名和登录路径，启动后端。
2. 重新编译，确认 Console 出现 `login.started`、operation=login 的请求及 `login.completed`。
3. 在后端检查 `code2Session` 是否成功，AppID 是否与客户端一致，用户会话是否真实建立；code 有时效且不能重复使用。
4. 无效会话访问受保护接口应返回 401，登录结果缺 token/openid 时客户端必须报错。

通过条件：真实手机取得后端会话，后续请求携带 Bearer，角色和用户归属正确。不要在接口未实现时手填 token 或 OpenID 绕过登录。

### 阶段 B：用管理员隔离验证视频链路

1. 在测试后端将指定 OpenID 配置为管理员，登录响应返回精确的 `role: "admin"`。
2. 先仅返回一个测试镜头，`canControl: false`，确认首页出现“管理员 · 全部镜头”。
3. 点击打开镜头，检查 streams 与 play 请求均成功，`play` 返回非空且手机可访问的地址。
4. 真机确认有持续画面，再加入第二个镜头测试切换。签名地址过期后点击刷新重新获取。

通过条件：未创建寄养记录的管理员能观看两个镜头并切换；普通用户不能借用该接口获得全部镜头。HTTP 200 和有 url 都不是“有画面”的充分条件。

### 阶段 C：验证普通用户寄养限制

1. 使用另一个普通账号登录，不要把管理员会话改成普通用户模拟。
2. 记录接口返回 `{"records":[]}`，首页应提示扫码寄养；直达直播页也不能播放或操作设备。
3. 由测试后端构造属于该测试用户的有效记录，验证 paid + active 显示对应镜头。该步骤只验证记录到视频的连接，不算真实支付成功。
4. 改为未支付、结束或退款状态，验证视频及设备请求在服务端拒绝；使用其他用户的 camera 参数同样返回 403。

通过条件：用户间隔离正确，未支付或失效记录不放行；最多 64 个镜头，编号与实际笼位一致。

### 阶段 D：扫码选笼位与微信支付

1. 由测试后端签发寄养二维码，type 必须是 `petshop-boarding`、version 为 1；不要把开门二维码当寄养二维码。
2. 在另一屏幕或打印件展示二维码，点击扫一扫寄养；代码只允许相机扫码，不支持从相册选图。
3. 核对返回的门店、可用笼位、一次寄养费用和时长规则，选择笼位后创建订单。
4. 确认订单最终金额，调用真机微信支付。使用隔离的测试商户/测试业务数据和明确的小额验证方案；本客户端没有假支付开关，真实支付可能产生真实扣款。
5. 验证后端收到可信微信通知或可信查单结果，完成金额等校验，再将订单置 paid/active，并写入有效寄养记录。
6. 客户端查询订单与有效记录，两者均生效后才跳转视频。通知延迟时使用“查询支付结果”，不要重复付款。
7. 测试支付取消、断网、重复点击、退出重进、并发抢同一笼位，以及关闭订单后的库存释放。

通过条件：真实支付链路、订单、笼位占用、寄养记录及对应镜头一致，未支付不能开权限；不能仅凭 `payment.client_completed` 判定完成。
待确认订单按 `boarding-order:<OpenID>` 存在本地。订单状态不明时不要随意清空缓存后重新付款；先在后端查单确认，必要时联系门店处理。

### 阶段 E：喂食、喂水与门禁

1. 先用设备网关的隔离测试模式验证协议，不连接真实投喂或门锁执行器。
2. 配置 feedPath、waterPath，普通用户需有效寄养记录。管理员目标镜头必须返回布尔值 `canControl: true`，false 或缺失时只可观看。
3. 管理员每次操作前会再次 GET streams，随后才 POST 设备指令；服务端仍要在执行时独立验证权限。
4. 在有人现场监护、剂量和频率受限的条件下连接实际设备，核对“提交一次”与实际执行一次。
5. 开门使用独立的 `petshop-door` 二维码，后端验证门店权限和短时凭证；accepted 仅受理，opened 才是门锁确认。
6. 测试设备离线、无权限、超时和凭证过期。结果不确定时先核查设备，禁止连续点击补发。

通过条件：操作用户、笼位/大门和设备对应正确；权限拒绝不执行；后端具有限流、去重和审计，设备回执可核实实际结果。

## 7. 常见故障定位

| 现象 | 优先检查 |
| --- | --- |
| 首页持续登录失败 | 占位域名、touristappid、登录路由未部署、code2Session AppID 不一致、响应缺 openid/token |
| “尚未配置有效的寄养接口” | app.js 的 boardingPath 为空、带末尾斜杠或不是相对路径；不是扫码才能修复 |
| 普通用户没镜头 | records 是否属于当前用户、字段是否为字符串、paymentStatus=paid 且 status=active；不要回退全部镜头 |
| 管理员仍被要求寄养 | 后端登录是否精确返回 role=admin；修改后重新登录，缓存会话不会自动刷新角色 |
| 管理员不能喂食喂水 | 当前镜头 canControl 是否为布尔 true、接口路径是否配置、操作前 streams 复查和设备后端是否授权 |
| HTTP 401 | 用户会话不存在/过期、代理丢失 Authorization；客户端清空会话后需重新登录 |
| HTTP 403 | 用户与设备/门店/订单权限不匹配，或寄养到期；不要改客户端绕过 |
| HTTP 404 或返回 HTML | 路由未实现、反向代理前缀错误、错误连接普通 Vigil 服务或站点首页 |
| request.network_failed | 合法域名、DNS、证书链、手机网络、防火墙、15 秒超时；没有 HTTP 状态时先查连接层 |
| play 200 但黑屏 | 检查流输出协议、编码、签名有效期、手机可达性、HTTPS 和 live-player 权限；查看播放器 code |
| 扫码内容被拒绝 | 二维码 JSON type/version/token 格式错误；真实凭证必须由后端签发，示例 token 不可用于业务 |
| 微信支付参数无效 | 订单是否 pending、amount 是否正整数分、payment 字段是否完整、签名与 AppID/商户配置是否一致 |
| 支付完成却没有视频 | 后端通知验签/查单、paid/active 状态、recordId/camera 和 records 是否同步 |
| 订单一直待确认 | 先后端查单；创建超时恢复需支持同一用户和 requestId 的幂等，不要新建另一个订单重复收费 |
| 按钮禁用或不显示 | 检查接口配置、当前角色、canControl、有效镜头、请求进行中及 10 秒冷却 |
| 提示指令已提交但设备未动 | 服务端受理不等于设备执行；查网关、设备在线状态、实际设备映射与回执 |

## 8. 日志收集与关联

微信开发者工具 Console 或真机调试中筛选 `[miniprogram]`。先记录复现时间、角色、操作步骤、operation、HTTP 状态、播放器 code，再向后端核对同时间段日志。

1. 登录：`login.started/completed/failed`。
2. 请求：用同一 `requestId` 关联 `request.started/completed/network_failed`，检查 `elapsedMs`、`statusCode` 和 operation。
3. 镜头：`cameras.loaded`、`video.camera_selected`、`video.player_failed`。
4. 支付：`payment.client_completed` 只是客户端回调；继续观察 `boarding.activation_pending/permissions_pending/activated`。
5. 设备：`device.submitted` 或 `door.result` 的 accepted 只代表受理，不代表实际完成。

客户端日志 requestId 没有发送为后端追踪头，不能直接据此在服务端检索；它也不是订单幂等 requestId。当前需用时间和操作关联服务端审计。
日志不持久化到文件，也没有自动上传平台；需要排查时保存脱敏日志片段。不要打印 Header、OpenID、登录 code、二维码 token、支付签名、订单原文或视频签名 URL。
Network 面板可能包含这些敏感信息，截图、导出或共享前务必脱敏，不把真实流地址放入公共在线播放站点。

## 9. 发布部署与最终验收

1. 先部署并验证正式后端、数据库迁移、HTTPS、支付通知和视频/设备网关；具体命令使用对应工程文档。
2. 将 app.js 指向正式 API，核对每个路径和 AppID，不携带测试身份、密钥或模拟支付数据。
3. 开启合法域名和证书校验，以普通用户和管理员分别做真机验收。
4. 开发者工具上传代码，在微信公众平台设置体验版和体验成员，完成审核及发布流程；上传客户端不会部署业务后端。
5. 记录小程序版本、后端版本、基础库版本、测试时间与设备，以及下面各项的真实证据。任何一项未验证均标记“未验证”，不得写成“已联调成功”。

| 验收项 | 当前证据要求 | 结果 |
| --- | --- | --- |
| 真机登录与角色 | 真实微信身份与后端会话对应 | 待填写 |
| 管理员浏览与控制 | 无寄养可看；canControl false/true 行为分别符合预期 | 待填写 |
| 普通用户隔离 | 无记录禁止访问，其他用户镜头/订单不可访问 | 待填写 |
| 寄养与真实支付 | 正确金额、微信通知、幂等、笼位占用及记录一致 | 待填写 |
| 视频 | 两个镜头持续播放、切换和签名刷新成功 | 待填写 |
| 喂食与喂水 | 实际设备执行一次，失败与权限拒绝不误操作 | 待填写 |
| 开门 | 实际授权、二维码防重放、门锁回执 | 待填写 |
| 到期/退款/撤权 | 服务端及时撤销访问与控制，库存正确释放 | 待填写 |

当前结论应保持为：客户端流程和模拟测试可检查，真实后端、微信平台、流媒体和设备的端到端联调尚未确认成功。