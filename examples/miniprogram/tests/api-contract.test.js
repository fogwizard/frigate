const assert = require("node:assert/strict");
const { test, beforeEach } = require("node:test");
const api = require("../utils/api");

let app;
let requests;
const qrToken = "integration_qr_token_123456";
const contracts = [
  { name: "login", invoke: () => api.loginWithCode("temporary-code"), path: "/session/login", method: "POST", data: { code: "temporary-code" }, public: true, response: { openid: "user", token: "token" } },
  { name: "streams", invoke: () => api.listStreams(), path: "/api/streams", method: "GET", response: { cameras: [] } },
  { name: "boarding records", invoke: () => api.boardingCameras(), path: "/boarding/records", method: "GET", response: { records: [] } },
  { name: "available cages", invoke: () => api.boardingCages(qrToken), path: "/boarding/cages", method: "POST", data: { qrToken }, response: { storeName: "test", cages: [] } },
  { name: "create order", invoke: () => api.createBoardingOrder(qrToken, "cage-1", "attempt-1"), path: "/boarding/orders", method: "POST", data: { qrToken, cageId: "cage-1", requestId: "attempt-1" }, response: { orderId: "order-1" } },
  { name: "query order", invoke: () => api.boardingOrder("order/1 ?"), path: "/boarding/orders/order%2F1%20%3F", method: "GET", response: { status: "pending" } },
  { name: "play", invoke: () => api.playUrl("room/1 ?"), path: "/api/streams/room%2F1%20%3F/play", method: "GET", response: { url: "https://media.example.com/test.flv" } },
  { name: "feed", invoke: () => api.feed("room-1"), path: "/devices/feed", method: "POST", data: { camera: "room-1" }, response: { success: true } },
  { name: "water", invoke: () => api.water("room-1"), path: "/devices/water", method: "POST", data: { camera: "room-1" }, response: { success: true } },
  { name: "door", invoke: () => api.openDoor(qrToken), path: "/doors/open", method: "POST", data: { qrToken }, response: { success: true, status: "accepted" } },
];

beforeEach(() => {
  requests = [];
  app = { globalData: {
    apiBase: "https://integration.example.com/gateway",
    loginPath: "/session/login", boardingPath: "/boarding",
    feedPath: "/devices/feed", waterPath: "/devices/water", doorPath: "/doors/open",
    openid: "user-a", sessionToken: "session-a", role: "user",
  } };
  global.getApp = () => app;
  global.wx = { request(options) { requests.push(options); } };
});

for (const contract of contracts) {
  test(`API contract: ${contract.name}`, async () => {
    const pending = contract.invoke();
    assert.equal(requests.length, 1);
    const request = requests[0];
    assert.equal(request.url, `https://integration.example.com/gateway${contract.path}`);
    assert.equal(request.method, contract.method);
    assert.equal(request.timeout, 15000);
    assert.deepEqual(request.data, contract.data);
    assert.deepEqual(request.header, contract.public
      ? { "content-type": "application/json" }
      : { "content-type": "application/json", Authorization: "Bearer session-a" });
    request.success({ statusCode: 200, data: contract.response });
    await pending;
  });
}

for (const statusCode of [400, 401, 403, 404, 409, 429, 500, 503]) {
  test(`HTTP ${statusCode} preserves the error status and never retries`, async () => {
    const pending = api.water("room-1");
    requests[0].success({ statusCode, data: { error: "backend-error" } });
    await assert.rejects(pending, (error) => {
      assert.equal(error.statusCode, statusCode);
      assert.match(error.message, statusCode === 401 ? /登录已失效/ : /backend-error/);
      return true;
    });
    assert.equal(requests.length, 1);
    assert.equal(app.globalData.sessionToken, statusCode === 401 ? "" : "session-a");
    assert.equal(app.globalData.openid, statusCode === 401 ? "" : "user-a");
  });
}

test("public login failure does not clear an unrelated existing user session", async () => {
  const pending = api.loginWithCode("expired-code");
  requests[0].success({ statusCode: 401, data: {} });
  await assert.rejects(pending, /登录/);
  assert.equal(app.globalData.sessionToken, "session-a");
  assert.equal(app.globalData.openid, "user-a");
});

test("network failure rejects a device request without retry or identity loss", async () => {
  const pending = api.feed("room-1");
  requests[0].fail({ errMsg: "request:fail timeout" });
  await assert.rejects(pending, (error) => error.code === "TIMEOUT");
  assert.equal(requests.length, 1);
  assert.equal(app.globalData.sessionToken, "session-a");
});

test("malformed boarding responses never grant camera access", async () => {
  for (const data of [null, {}, { records: null }, { records: {} }, { records: "paid" }]) {
    const pending = api.boardingCameras();
    requests[requests.length - 1].success({ statusCode: 200, data });
    await assert.rejects(pending, /寄养记录格式错误/);
  }
});

test("boarding base path must be a relative prefix without trailing slash", async () => {
  for (const path of ["", "/boarding/", "https://other.example/boarding", "//other.example/boarding", null]) {
    app.globalData.boardingPath = path;
    await assert.rejects(api.boardingCameras(), /寄养接口/);
  }
  assert.equal(requests.length, 0);
});

test("revoked admin permission aborts the command before any device POST", async () => {
  app.globalData.role = "admin";
  const pending = api.feed("room-1");
  assert.equal(requests[0].method, "GET");
  requests[0].success({ statusCode: 401, data: {} });
  await assert.rejects(pending, /登录已失效/);
  assert.equal(requests.length, 1);
  assert.equal(app.globalData.role, "user");
});