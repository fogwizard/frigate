const assert = require("node:assert/strict");
const { test, beforeEach } = require("node:test");
const api = require("../utils/api");

let app;
let requests;
let definition;
let appDefinition;

global.Page = (page) => { definition = page; };
require("../pages/live/live");
global.App = (application) => { appDefinition = application; };
require("../app");

function createPage() {
  return {
    ...definition,
    data: { ...definition.data, cameras: [{ camera: "camera-1", label: "01号" }], camera: "camera-1", feedConfigured: true, waterConfigured: true },
    setData(update) { Object.assign(this.data, update); },
  };
}

beforeEach(() => {
  app = { globalData: { apiBase: "https://api.example.com", sessionToken: "test-token", openid: "openid-a", feedPath: "/device/feed", waterPath: "/device/water" } };
  requests = [];
  global.getApp = () => app;
  global.wx = {
    request(options) { requests.push(options); },
    showToast() {},
    showModal(options) {
      options.success({ confirm: true });
      if (options.complete) options.complete();
    },
  };
});

test("stream requests retain GET and encoded camera names", async () => {
  const pending = api.playUrl("front door/1");
  assert.equal(requests[0].method, "GET");
  assert.equal(requests[0].url, "https://api.example.com/api/streams/front%20door%2F1/play");
  requests[0].success({ statusCode: 200, data: { url: "https://media.example.com/live.flv" } });
  assert.ok((await pending).url);
});

test("feeding requires a configured relative endpoint and a camera", async () => {
  for (const path of ["", "https://other.example/feed", "//other.example/feed"]) {
    app.globalData.feedPath = path;
    await assert.rejects(api.feed("camera-1"), /喂食接口/);
  }
  app.globalData.feedPath = "/device/feed";
  await assert.rejects(api.feed(""), /设备/);
  assert.equal(requests.length, 0);
});

test("feeding posts the camera with the existing authentication header", async () => {
  const pending = api.feed("camera-1");
  assert.equal(requests[0].method, "POST");
  assert.deepEqual(requests[0].data, { camera: "camera-1" });
  assert.equal(requests[0].header.Authorization, "Bearer test-token");
  requests[0].success({ statusCode: 200, data: { success: true } });
  await pending;
});

test("feeding rejects HTTP and business errors", async () => {
  const http = api.feed("camera-1");
  requests[0].success({ statusCode: 403, data: { error: "Forbidden" } });
  await assert.rejects(http, /Forbidden/);
  const business = api.feed("camera-1");
  requests[1].success({ statusCode: 200, data: { success: false, message: "Device offline" } });
  await assert.rejects(business, /Device offline/);
});

test("feeding cancellation or an unconfigured service sends no request", () => {
  const page = createPage();
  wx.showModal = (options) => {
    options.success({ confirm: false });
    options.complete();
  };
  page.manualFeed();
  assert.equal(page.confirming, false);
  page.data.feedConfigured = false;
  page.manualFeed();
  assert.equal(requests.length, 0);
});

test("feeding prevents duplicate submissions and enters cooldown", async () => {
  const page = createPage();
  page.manualFeed();
  page.manualFeed();
  assert.equal(requests.length, 1);
  assert.equal(page.data.feeding, true);
  requests[0].success({ statusCode: 200, data: { success: true } });
  await new Promise(setImmediate);
  assert.equal(page.data.feeding, false);
  assert.equal(page.data.cooldown, true);
  assert.equal(page.data.feedStatus, "喂食指令已提交");
  page.manualFeed();
  assert.equal(requests.length, 1);
  page.onUnload();
});

test("network failure reports uncertain outcome and never retries", async () => {
  const page = createPage();
  page.manualFeed();
  requests[0].fail({ errMsg: "request:fail timeout" });
  await new Promise(setImmediate);
  assert.equal(requests.length, 1);
  assert.equal(page.data.feeding, false);
  assert.match(page.data.feedStatus, /核实设备状态/);
  assert.equal(page.data.cooldown, true);
  page.onUnload();
});

test("player state distinguishes playback, buffering, and interruption", () => {
  const page = createPage();
  page.onState({ detail: { code: 2004 } });
  assert.equal(page.data.playing, true);
  page.onState({ detail: { code: 2007 } });
  assert.equal(page.data.playing, false);
  page.onState({ detail: { code: -2301 } });
  assert.match(page.data.error, /中断/);
});

test("watering posts to its own endpoint with camera and authentication", async () => {
  const pending = api.water("camera-1");
  assert.equal(requests[0].url, "https://api.example.com/device/water");
  assert.equal(requests[0].method, "POST");
  assert.deepEqual(requests[0].data, { camera: "camera-1" });
  assert.equal(requests[0].header.Authorization, "Bearer test-token");
  requests[0].success({ statusCode: 200, data: { success: true } });
  await pending;
});

test("watering requires a relative endpoint and a camera", async () => {
  for (const path of ["", "https://other.example/water", "//other.example/water"]) {
    app.globalData.waterPath = path;
    await assert.rejects(api.water("camera-1"), /喂水接口/);
  }
  app.globalData.waterPath = "/device/water";
  await assert.rejects(api.water(""), /设备/);
  assert.equal(requests.length, 0);
});

test("watering rejects HTTP and business errors", async () => {
  const http = api.water("camera-1");
  requests[0].success({ statusCode: 403, data: { error: "Forbidden" } });
  await assert.rejects(http, /Forbidden/);
  const business = api.water("camera-1");
  requests[1].success({ statusCode: 200, data: { success: false } });
  await assert.rejects(business, /喂水请求未被接受/);
});

test("watering cancellation or an unavailable device sends no request", () => {
  const page = createPage();
  wx.showModal = (options) => {
    assert.equal(options.title, "确认手动喂水");
    options.success({ confirm: false });
    options.complete();
  };
  page.manualWater();
  assert.equal(page.confirming, false);
  page.data.waterConfigured = false;
  page.manualWater();
  page.data.waterConfigured = true;
  page.data.camera = "";
  page.manualWater();
  assert.equal(requests.length, 0);
});

test("watering prevents duplicates and keeps its cooldown independent from feeding", async () => {
  const page = createPage();
  page.data.cooldown = true;
  page.manualWater();
  page.manualWater();
  assert.equal(requests.length, 1);
  assert.equal(page.data.watering, true);
  assert.equal(page.data.feeding, false);
  requests[0].success({ statusCode: 200, data: { success: true } });
  await new Promise(setImmediate);
  assert.equal(page.data.watering, false);
  assert.equal(page.data.waterCooldown, true);
  assert.equal(page.data.waterStatus, "喂水指令已提交");
  assert.equal(page.data.feedStatus, "");
  page.manualWater();
  assert.equal(requests.length, 1);
  page.onUnload();
});

test("watering timeout reports uncertain outcome without retrying", async () => {
  const page = createPage();
  page.manualWater();
  requests[0].fail({ errMsg: "request:fail timeout" });
  await new Promise(setImmediate);
  assert.equal(requests.length, 1);
  assert.equal(page.data.watering, false);
  assert.match(page.data.waterStatus, /核实设备状态/);
  assert.equal(page.data.feedStatus, "");
  assert.equal(page.data.waterCooldown, true);
  page.onUnload();
});

test("page derives feeding and watering availability independently", () => {
  const page = createPage();
  app.globalData.waterPath = "";
  page.onShow();
  assert.equal(page.data.feedConfigured, true);
  assert.equal(page.data.waterConfigured, false);
  app.globalData.waterPath = "/device/water";
  app.globalData.feedPath = "";
  page.onShow();
  assert.equal(page.data.feedConfigured, false);
  assert.equal(page.data.waterConfigured, true);
});

test("watering completion after unload does not update the page", async () => {
  const page = createPage();
  page.manualWater();
  page.onUnload();
  page.setData = () => assert.fail("Unexpected update after unload");
  requests[0].success({ statusCode: 200, data: { success: true } });
  await new Promise(setImmediate);
  assert.equal(page.waterTimer, undefined);
});

test("login exchanges only a wx.login code and coalesces concurrent callers", async () => {
  app = { ...appDefinition, globalData: { ...appDefinition.globalData } };
  let loginCalls = 0;
  wx.login = (options) => {
    loginCalls += 1;
    options.success({ code: "one-time-code" });
  };
  const first = app.login();
  const second = app.login();
  assert.equal(first, second);
  await new Promise(setImmediate);
  assert.equal(requests[0].method, "POST");
  assert.deepEqual(requests[0].data, { code: "one-time-code" });
  assert.equal(requests[0].header.Authorization, undefined);
  requests[0].success({ statusCode: 200, data: { openid: "openid-a", token: "user-token" } });
  assert.equal(await first, "openid-a");
  assert.equal(await app.login(), "openid-a");
  assert.equal(loginCalls, 1);
  assert.equal(app.globalData.sessionToken, "user-token");
});

test("device requests require a user session and never fall back to a shared token", async () => {
  app.globalData.sessionToken = "";
  app.globalData.apiToken = "shared-token";
  await assert.rejects(api.listStreams(), /登录/);
  await assert.rejects(api.playUrl("camera-1"), /登录/);
  await assert.rejects(api.feed("camera-1"), /登录/);
  await assert.rejects(api.water("camera-1"), /登录/);
  assert.equal(requests.length, 0);
});

test("401 clears identity without retrying a device command", async () => {
  const pending = api.water("camera-1");
  requests[0].success({ statusCode: 401, data: {} });
  await assert.rejects(pending, /登录已失效/);
  assert.equal(app.globalData.sessionToken, "");
  assert.equal(app.globalData.openid, "");
  assert.equal(requests.length, 1);
});

test("invalid login responses do not establish identity and allow retry", async () => {
  app = { ...appDefinition, globalData: { ...appDefinition.globalData } };
  wx.login = (options) => options.success({ code: "code" });
  const pending = app.login();
  await new Promise(setImmediate);
  requests[0].success({ statusCode: 200, data: { openid: "openid-a" } });
  await assert.rejects(pending, /openid 和 token/);
  assert.equal(app.globalData.openid, "");
  wx.login = (options) => options.fail();
  await assert.rejects(app.login(), /微信登录失败/);
});

test("camera normalization preserves assigned numbers and caps unique cameras at 64", () => {
  const { normalizeCameras, MAX_CAMERAS } = require("../utils/cameras");
  assert.equal(MAX_CAMERAS, 64);
  const cameras = normalizeCameras({ cameras: [
    { camera: "camera-1", number: 23, label: "寄养 A" },
    "camera-1", null, {}, "",
    ...Array.from({ length: 70 }, (_, index) => `device-${index}`),
  ] });
  assert.equal(cameras.length, 64);
  assert.deepEqual(cameras[0], { camera: "camera-1", number: "23", label: "23号 · 寄养 A" });
  assert.equal(cameras[1].number, "02");
  assert.deepEqual(normalizeCameras({ cameras: [] }), []);
  assert.throws(() => normalizeCameras({}), /格式错误/);
});