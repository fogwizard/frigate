const assert = require("node:assert/strict");
const { test, beforeEach, afterEach } = require("node:test");
const api = require("../utils/api");
const { parseDoorCode } = require("../utils/door");

let definition;
global.Page = (page) => { definition = page; };
require("../pages/index/index");
let app;
let page;
let requests;
let scanCalls;
let modalCalls;
const qrToken = "door_challenge_123456789";
const code = JSON.stringify({ type: "petshop-door", version: 1, token: qrToken });

beforeEach(() => {
  requests = [];
  scanCalls = 0;
  modalCalls = 0;
  app = {
    globalData: { apiBase: "https://api.example.com", doorPath: "/api/doors/open", openid: "openid-a", sessionToken: "user-token" },
    async login() { return this.globalData.openid; },
  };
  global.getApp = () => app;
  global.wx = {
    request(options) { requests.push(options); },
    showToast() {},
    showModal(options) { modalCalls += 1; options.success({ confirm: true }); },
    scanCode(options) {
      scanCalls += 1;
      assert.equal(options.onlyFromCamera, true);
      assert.deepEqual(options.scanType, ["qrCode"]);
      options.success({ result: code });
    },
  };
  page = { ...definition, data: { ...definition.data, doorConfigured: true }, setData(update) { Object.assign(this.data, update); } };
});

afterEach(() => { page.onUnload(); });

test("door QR accepts only versioned door challenges, never URLs", () => {
  assert.equal(parseDoorCode(code), qrToken);
  for (const invalid of ["https://other.example/open", "", "x".repeat(2049), "null",
    JSON.stringify({ type: "petshop-door", version: 2, token: qrToken }),
    JSON.stringify({ type: "petshop-door", version: 1, token: "short" }),
    JSON.stringify({ type: "payment", version: 1, token: qrToken })]) {
    assert.throws(() => parseDoorCode(invalid), /二维码/);
  }
});

test("openDoor requires an endpoint, valid challenge, and session", async () => {
  for (const path of ["", "https://other.example/open", "//other.example/open"]) {
    app.globalData.doorPath = path;
    await assert.rejects(api.openDoor(qrToken), /开门接口/);
  }
  app.globalData.doorPath = "/api/doors/open";
  await assert.rejects(api.openDoor("invalid"), /凭证/);
  app.globalData.sessionToken = "";
  await assert.rejects(api.openDoor(qrToken), /登录/);
  assert.equal(requests.length, 0);
});

test("door request uses the fixed backend and user token without a client openid", async () => {
  const pending = api.openDoor(qrToken);
  assert.equal(requests[0].url, "https://api.example.com/api/doors/open");
  assert.equal(requests[0].method, "POST");
  assert.equal(requests[0].header.Authorization, "Bearer user-token");
  assert.deepEqual(requests[0].data, { qrToken });
  requests[0].success({ statusCode: 200, data: { success: true, status: "accepted" } });
  assert.equal((await pending).status, "accepted");
});

test("empty or refused responses do not imply an open door", async () => {
  for (const data of [undefined, {}, { success: false }, { success: true }, { success: true, status: "offline" }]) {
    const pending = api.openDoor(qrToken);
    requests[requests.length - 1].success({ statusCode: 200, data });
    await assert.rejects(pending);
  }
});

test("scan flow logs in, confirms, blocks duplicate taps and cools down", async () => {
  let loggedIn = false;
  app.login = async () => { loggedIn = true; };
  wx.scanCode = (options) => {
    assert.equal(loggedIn, true);
    scanCalls += 1;
    options.success({ result: code });
  };
  const pending = page.scanOpenDoor();
  await page.scanOpenDoor();
  await new Promise(setImmediate);
  assert.equal(modalCalls, 2);
  assert.equal(scanCalls, 1);
  assert.equal(requests.length, 1);
  requests[0].success({ statusCode: 200, data: { success: true, status: "accepted" } });
  await pending;
  assert.match(page.data.doorStatus, /已受理/);
  assert.equal(page.data.doorBusy, false);
  assert.equal(page.data.doorCooldown, true);
  await page.scanOpenDoor();
  assert.equal(requests.length, 1);
});

test("confirmed hardware result is displayed distinctly", async () => {
  const pending = page.scanOpenDoor();
  await new Promise(setImmediate);
  requests[0].success({ statusCode: 200, data: { success: true, status: "opened" } });
  await pending;
  assert.equal(page.data.doorStatus, "门锁已确认开门");
});

test("declining identity confirmation never logs in or scans", async () => {
  app.login = () => assert.fail("Unexpected login");
  wx.showModal = (options) => options.success({ confirm: false });
  await page.scanOpenDoor();
  assert.equal(scanCalls, 0);
  assert.equal(requests.length, 0);
  assert.equal(page.data.doorBusy, false);
});

test("login failure blocks scanner and device request", async () => {
  app.login = async () => { throw new Error("微信登录失败"); };
  await page.scanOpenDoor();
  assert.equal(scanCalls, 0);
  assert.equal(requests.length, 0);
  assert.match(page.data.doorStatus, /登录失败/);
});

test("scan cancellation and camera denial never send commands", async () => {
  wx.scanCode = (options) => options.fail({ errMsg: "scanCode:fail cancel" });
  await page.scanOpenDoor();
  assert.equal(page.data.doorStatus, "已取消扫码");
  wx.scanCode = (options) => options.fail({ errMsg: "scanCode:fail auth deny" });
  await page.scanOpenDoor();
  assert.match(page.data.doorStatus, /相机权限/);
  assert.equal(requests.length, 0);
});

test("invalid QR or final confirmation cancellation sends no command", async () => {
  wx.scanCode = (options) => options.success({ result: "https://other.example/open" });
  await page.scanOpenDoor();
  assert.match(page.data.doorStatus, /二维码/);
  wx.scanCode = (options) => options.success({ result: code });
  wx.showModal = (options) => options.success({ confirm: options.title !== "确认申请开门" });
  await page.scanOpenDoor();
  assert.equal(page.data.doorStatus, "已取消开门");
  assert.equal(requests.length, 0);
});

test("server denials are not retried and do not report success", async () => {
  const pending = page.scanOpenDoor();
  await new Promise(setImmediate);
  requests[0].success({ statusCode: 403, data: { error: "Forbidden" } });
  await pending;
  assert.equal(page.data.doorStatus, "没有此门店的开门权限");
  assert.equal(requests.length, 1);
  assert.equal(page.data.doorCooldown, true);
});

test("timeouts have uncertain results, no retries and a cooldown", async () => {
  const pending = page.scanOpenDoor();
  await new Promise(setImmediate);
  requests[0].fail({ errMsg: "request:fail timeout" });
  await pending;
  assert.match(page.data.doorStatus, /核实门锁状态/);
  assert.equal(requests.length, 1);
  assert.equal(page.data.doorCooldown, true);
});

test("unconfigured service does not start scanner", async () => {
  page.data.doorConfigured = false;
  await page.scanOpenDoor();
  assert.equal(scanCalls, 0);
  assert.equal(requests.length, 0);
});

test("leaving during login prevents scanning or late updates", async () => {
  let finishLogin;
  app.login = () => new Promise((resolve) => { finishLogin = resolve; });
  const pending = page.scanOpenDoor();
  await new Promise(setImmediate);
  page.onUnload();
  page.setData = () => assert.fail("Unexpected update after unload");
  finishLogin();
  await pending;
  assert.equal(scanCalls, 0);
  assert.equal(requests.length, 0);
});