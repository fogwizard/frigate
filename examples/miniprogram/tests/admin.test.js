const assert = require("node:assert/strict");
const { test, beforeEach } = require("node:test");
const api = require("../utils/api");
let application;
global.App = (definition) => { application = definition; };
require("../app");
let live;
global.Page = (definition) => { live = definition; };
require("../pages/live/live");
let app;
let requests;

beforeEach(() => {
  requests = [];
  app = { globalData: { apiBase: "https://api.example.com", boardingPath: "", role: "admin", openid: "admin-a", sessionToken: "admin-token" }, async login() {} };
  global.getApp = () => app;
  global.wx = { request(options) { requests.push(options); }, setNavigationBarTitle() {} };
});

test("admin cameras do not require a boarding endpoint or record", async () => {
  const pending = api.accessibleCameras();
  assert.equal(requests[0].url, "https://api.example.com/api/streams");
  assert.equal(requests[0].header.Authorization, "Bearer admin-token");
  requests[0].success({ statusCode: 200, data: { cameras: ["room-1", { camera: "room-2", canControl: true }] } });
  const data = await pending;
  assert.equal(data.isAdmin, true);
  assert.equal(data.cameras.length, 2);
  assert.equal(data.cameras[0].canControl, false);
  assert.equal(data.cameras[1].canControl, true);
});

test("ordinary users still require paid boarding and never request all streams", async () => {
  app.globalData.role = "user";
  app.globalData.boardingPath = "/api/boarding";
  const pending = api.accessibleCameras();
  assert.match(requests[0].url, /\/boarding\/records$/);
  requests[0].success({ statusCode: 200, data: { records: [] } });
  assert.deepEqual(await pending, { cameras: [], isAdmin: false });
  assert.equal(requests.length, 1);
});

test("admin rejection never falls back and expired sessions reset the role", async () => {
  const forbidden = api.accessibleCameras();
  requests[0].success({ statusCode: 403, data: {} });
  await assert.rejects(forbidden, /403/);
  assert.equal(requests.length, 1);
  const expired = api.accessibleCameras();
  requests[1].success({ statusCode: 401, data: {} });
  await assert.rejects(expired, /登录/);
  assert.equal(app.globalData.role, "user");
});

test("only exact server admin role enables the login exception", async () => {
  for (const role of ["admin", "user", "ADMIN", true, undefined]) {
    app = { ...application, globalData: { ...application.globalData } };
    wx.login = (options) => options.success({ code: "code" });
    const pending = app.login();
    await new Promise(setImmediate);
    requests[requests.length - 1].success({ statusCode: 200, data: { openid: "user", token: "token", role } });
    await pending;
    assert.equal(app.globalData.role, role === "admin" ? "admin" : "user");
  }
});

test("admin controls follow the selected camera and do not leak across switches", async () => {
  const page = { ...live, data: { ...live.data }, setData(update) { Object.assign(this.data, update); } };
  const pending = page.onLoad({});
  await new Promise(setImmediate);
  requests[0].success({ statusCode: 200, data: { cameras: ["room-1", { camera: "room-2", canControl: true }] } });
  await new Promise(setImmediate);
  assert.match(requests[1].url, /room-1\/play$/);
  requests[1].success({ statusCode: 200, data: { url: "room-1.flv" } });
  await pending;
  assert.equal(page.data.isAdmin, true);
  assert.equal(page.data.canControl, false);
  page.data.feedConfigured = true;
  page.data.waterConfigured = true;
  wx.showModal = () => assert.fail("Read-only admin should not control devices");
  page.manualFeed();
  page.manualWater();
  const switching = page.selectCamera({ detail: { value: "1" } });
  requests[2].success({ statusCode: 200, data: { url: "room-2.flv" } });
  await switching;
  assert.equal(page.data.canControl, true);
  let confirmations = 0;
  wx.showModal = (options) => {
    confirmations += 1;
    options.success({ confirm: false });
    options.complete();
  };
  page.manualFeed();
  page.manualWater();
  assert.equal(confirmations, 2);
  assert.equal(requests.length, 3);
  const back = page.selectCamera({ detail: { value: "0" } });
  requests[3].success({ statusCode: 200, data: { url: "room-1.flv" } });
  await back;
  assert.equal(page.data.canControl, false);
  page.manualFeed();
  assert.equal(confirmations, 2);
});

test("admin API rejects absent false and nonboolean control permissions", async () => {
  app.globalData.feedPath = "/feed";
  app.globalData.waterPath = "/water";
  for (const canControl of [false, undefined, "true", 1]) {
    for (const action of ["feed", "water"]) {
      const pending = api[action]("room-1");
      requests[requests.length - 1].success({ statusCode: 200, data: { cameras: [{ camera: "room-1", canControl }] } });
      await assert.rejects(pending, /管理员无此镜头/);
    }
  }
  assert.equal(requests.filter((request) => request.method === "POST").length, 0);
});

test("admin session blocks page commands even if local control flags are stale", () => {
  const page = { ...live, data: { ...live.data, isAdmin: false, canControl: true,
    camera: "room-1", cameras: [{ camera: "room-1", canControl: false }],
    feedConfigured: true, waterConfigured: true },
  setData(update) { Object.assign(this.data, update); } };
  wx.showModal = () => assert.fail("Admin must not reach command confirmation");
  page.manualFeed();
  page.manualWater();
  assert.equal(requests.length, 0);
});

test("admin feed and water recheck server permission before posting", async () => {
  app.globalData.feedPath = "/feed";
  app.globalData.waterPath = "/water";
  for (const action of ["feed", "water"]) {
    const offset = requests.length;
    const pending = api[action]("room-2");
    assert.match(requests[offset].url, /\/api\/streams$/);
    requests[offset].success({ statusCode: 200, data: { cameras: [{ camera: "room-2", canControl: true }] } });
    await new Promise(setImmediate);
    assert.equal(requests[offset + 1].method, "POST");
    assert.deepEqual(requests[offset + 1].data, { camera: "room-2" });
    requests[offset + 1].success({ statusCode: 200, data: { success: true } });
    await pending;
  }
});

test("permission to control another camera never grants this camera access", async () => {
  app.globalData.feedPath = "/feed";
  const pending = api.feed("room-1");
  requests[0].success({ statusCode: 200, data: { cameras: [{ camera: "room-2", canControl: true }] } });
  await assert.rejects(pending, /管理员无此镜头/);
  assert.equal(requests.length, 1);
});