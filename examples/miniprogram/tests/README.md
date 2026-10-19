# 单元测试与集成开发检查

## 运行入口

不需要安装 npm 包，使用支持 `node:test` 的 Node.js，建议使用受支持的 Node.js LTS。
在工作区 `examples` 目录执行：

```powershell
node ./miniprogram/tests/run.js
```

在小程序目录执行：

```powershell
node ./tests/run.js
```

运行入口根据自身目录自动发现所有 `*.test.js`，不依赖当前工作目录；新增测试文件后无需维护文件清单。
子进程直接使用当前 Node.js，不调用 shell，不读取真实业务配置或发起真实网络请求。
测试失败时返回非零退出码，可作为后端联调前的 CI 检查项。
默认使用 dot 输出以减少模拟业务日志噪声，详细输出和汇总可使用：

```powershell
node ./miniprogram/tests/run.js --test-reporter=spec
node ./miniprogram/tests/run.js --test-reporter=tap
```

只检查接口契约或支付页面：

```powershell
node --test --test-reporter=spec ./miniprogram/tests/api-contract.test.js
node --test --test-reporter=spec ./miniprogram/tests/boarding-page.test.js
```

也可用 Node.js 的 `--test-name-pattern` 筛选测试名称；筛选后跳过的测试不算全量回归通过。

## 覆盖矩阵

| 测试文件 | 检查内容 | 联调时对应检查 |
| --- | --- | --- |
| [api-contract.test.js](api-contract.test.js) | 10 类接口的路径、方法、请求体、Bearer、15 秒超时；常见 HTTP 错误码与无自动重试 | 后端路由及响应协议是否与客户端一致 |
| [timeout.test.js](timeout.test.js) | 请求与登录 15 秒超时、主动中止、迟到回调、同步平台异常、视频首帧与缓冲 20 秒超时、退出清理及加载状态恢复 | 网络无响应、视频黑屏及超时后的手动恢复 |
| [admin.test.js](admin.test.js) | 管理员全镜头、canControl 严格布尔校验、控制前复核、跨镜头隔离 | 后端角色与逐设备权限校验 |
| [boarding.test.js](boarding.test.js) | 寄养二维码、可用笼位及价格、支付参数白名单、paid + active 记录 | 二维码签发、价格和记录字段 |
| [boarding-page.test.js](boarding-page.test.js) | 支付前确认、服务端生效后跳转、取消、关闭/退款、409 冲突、401、超时、幂等标识恢复、按用户存储与卸载保护 | 订单生命周期、通知延迟和恢复流程 |
| [cameras.test.js](cameras.test.js) | 64 镜头上限、进入视频前复查、空记录、切换时旧响应隔离、操作期间锁定镜头 | 有效记录到视频与设备的绑定 |
| [feeding.test.js](feeding.test.js) | 微信登录、喂食喂水请求、操作确认、独立冷却、失败与权限失效 | 登录会话和真实设备操作 |
| [door.test.js](door.test.js) | 开门二维码格式、用户确认、拒绝/取消、单次请求及不确定结果保护 | 门店授权、二维码防重放和设备回执 |
| [logger.test.js](logger.test.js) | 日志白名单和敏感值不泄露 | 真机日志可定位且不暴露凭证 |

HTTP 错误码测试包括 400、401、403、404、409、429、500、503；401 会清除客户端会话，其他错误不会自动重试或清除无关身份。
接口契约测试使用独立的测试域名和路径，验证配置拼接，不要求开发者修改 [../app.js](../app.js) 中的实际环境配置。

## 对后端响应的关键要求

1. 登录返回非空字符串 `openid`、`token`，管理员必须精确返回 `role: "admin"`。
2. 普通用户返回寄养 `records`，有效记录必须具备 `id`、`camera`，且 `paymentStatus=paid,status=active`。
3. 管理员镜头 `canControl` 仅接受布尔 `true`，字符串 `"true"` 不开放操作。
4. 下单请求不提交价格；提交的 `requestId` 用于恢复同一创建尝试，不得因此重复生成收费订单。
5. 支付成功回调本身不放行视频；订单状态和有效记录均确认后才跳转。
6. 订单终止使用 `closed/cancelled/refunded`；客户端收到终止状态后清理待确认订单。
7. 库存冲突或创建超时会保留原创建尝试。测试只证明不重复付款和不误放权限，不代表冲突自动解决；后端需要提供同一请求标识的可查询结果或人工处理渠道。
8. 订单查询、设备调用和播放接口仍必须逐次校验用户权限，客户端单元测试不能验证服务端访问控制。

完整接口字段见 [../README.md](../README.md)，真实部署与逐阶段验收见 [../DEBUGGING.md](../DEBUGGING.md)。

## 新增测试的约定

- 文件名使用 `*.test.js`，使用 Node.js 内置 `node:test` 和 `node:assert/strict`。
- 通过 `beforeEach` 重建 `wx`、`getApp`、页面状态和测试响应，不依赖真实账号、服务端或设备。
- 模拟 `Page`、`App` 注册时在各自测试进程内捕获定义；不要使用 `--test-isolation=none` 将这些全局注册混在一起。
- 每个异步页面操作都等待完成；不要让未处理的 Promise 拒绝在测试结束后才触发。
- 需要推进 Promise 链时使用 `setImmediate`，不依赖固定延时。产生冷却计时器的测试结束前应调用页面 `onUnload()` 清理。
- 除正确返回值外，检查不应发生的副作用：不请求设备、不付款、不跳转、不重复下单、不在卸载后更新页面。
- 不使用真实 OpenID、token、二维码或支付签名；示例测试凭证不具备真实业务权限。

## 不能替代的集成验收

这些测试模拟微信 API 和 HTTP 响应，运行通过只证明客户端在给定输入下符合断言，**不代表本程序已经联调成功**。
以下项目必须另行验证：

- 微信 `code2Session`、合法域名、TLS 和真机会话。
- 数据库事务、同一笼位并发竞争、服务端持久化幂等与用户隔离。
- 微信商户配置、支付签名、通知验签、真实扣款、退款与金额核对。
- HTTP-FLV 实际输出、编码兼容性、组件资质、签名地址有效期及持续播放。
- 喂食、喂水、开门硬件实际执行、限流、防重放、审计及权限即时撤销。
- 原生 WXML/WXSS 布局、扫码相机权限、微信支付界面和手机生命周期。

建议顺序：全量单元测试 -> 后端接口契约逐项对照 -> 测试环境真机联调 -> 受控设备验收 -> 正式发布检查。不要通过改变测试断言或伪造 paid/active 来证明真实收款与设备执行。