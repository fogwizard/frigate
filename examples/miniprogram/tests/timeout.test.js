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
let request;
let aborts;

beforeEach(() => {
  request = undefined;
  aborts = 0;
  app = { globalData: { apiBase: "https://test.example", sessionToken: "token", openid: "user", role: "user", waterPath: "/water" } };
  global.getApp = () => app;
  global.wx = { request(options) { request = options; return { abort() { aborts += 1; options.fail({ errMsg: "request:fail abort" }); } }; } };
});

test("request watchdog aborts once and ignores late success and 401 callbacks", async (context) => {
  context.mock.timers.enable({ apis: ["setTimeout"] });
  const output = [];
  context.mock.method(console, "warn", (...args) => output.push(args));
  const pending = api.water("camera");
  const rejected = assert.rejects(pending, (error) => error.code === "TIMEOUT");
  context.mock.timers.tick(15000);
  await rejected;
  request.success({ statusCode: 401, data: {} });
  request.success({ statusCode: 200, data: { success: true } });
  context.mock.timers.tick(30000);
  assert.equal(aborts, 1);
  assert.equal(app.globalData.sessionToken, "token");
  assert.equal(output.filter((entry) => entry[0].includes("request.timed_out")).length, 1);
  assert.equal(output[0][1].outcome, "timeout");
});

test("native timeout and watchdog share one terminal result", async (context) => {
  context.mock.timers.enable({ apis: ["setTimeout"] });
  const pending = api.playUrl("camera");
  request.fail({ errMsg: "request:fail timeout secret-url" });
  await assert.rejects(pending, (error) => error.code === "TIMEOUT" && !error.message.includes("secret"));
  context.mock.timers.tick(20000);
  assert.equal(aborts, 0);
});

test("successful requests clear their watchdog", async (context) => {
  context.mock.timers.enable({ apis: ["setTimeout"] });
  const pending = api.playUrl("camera");
  request.success({ statusCode: 200, data: { url: "test.flv" } });
  assert.equal((await pending).url, "test.flv");
  context.mock.timers.tick(20000);
  assert.equal(aborts, 0);
});

test("synchronous platform exception clears watchdog without leaking raw errors", async (context) => {
  context.mock.timers.enable({ apis: ["setTimeout"] });
  wx.request = () => { throw new Error("secret-platform-details"); };
  await assert.rejects(api.playUrl("camera"), /无法发起请求/);
  context.mock.timers.tick(20000);
  assert.equal(aborts, 0);
});

test("login timeout releases concurrent callers and ignores a late code before retry", async (context) => {
  context.mock.timers.enable({ apis: ["setTimeout"] });
  app = { ...application, globalData: { ...application.globalData } };
  let login;
  wx.login = (options) => { login = options; };
  const pending = app.login();
  assert.equal(app.login(), pending);
  const rejected = assert.rejects(pending, (error) => error.code === "TIMEOUT");
  context.mock.timers.tick(15000);
  await rejected;
  assert.equal(app.loginPromise, null);
  login.success({ code: "late-code" });
  await new Promise(setImmediate);
  assert.equal(request, undefined);
  const retry = app.login();
  login.success({ code: "new-code" });
  await new Promise(setImmediate);
  request.success({ statusCode: 200, data: { openid: "user", token: "token" } });
  assert.equal(await retry, "user");
});

function videoPage() {
  return { ...live, data: { ...live.data, camera: "camera", cameras: [{ camera: "camera", label: "01" }] },
    setData(update) { Object.assign(this.data, update); } };
}

test("missing first frame times out once and a late player event cannot revive it", async (context) => {
  context.mock.timers.enable({ apis: ["setTimeout"] });
  const page = videoPage();
  const pending = page.refresh();
  request.success({ statusCode: 200, data: { url: "test.flv" } });
  await pending;
  context.mock.timers.tick(20000);
  assert.equal(page.data.flvUrl, "");
  assert.match(page.data.error, /超时/);
  page.onState({ detail: { code: 2004 } });
  assert.equal(page.data.playing, false);
  const retry = page.refresh();
  request.success({ statusCode: 200, data: { url: "new.flv" } });
  await retry;
  page.onState({ detail: { code: 2004 } });
  context.mock.timers.tick(20000);
  assert.equal(page.data.playing, true);
  assert.equal(page.data.error, "");
});

test("repeated buffering does not postpone timeout and recovery clears its timer", (context) => {
  context.mock.timers.enable({ apis: ["setTimeout"] });
  const page = videoPage();
  page.data.flvUrl = "test.flv";
  page.onState({ detail: { code: 2004 } });
  page.onState({ detail: { code: 2007 } });
  context.mock.timers.tick(10000);
  page.onState({ detail: { code: 2007 } });
  context.mock.timers.tick(10000);
  assert.match(page.data.error, /超时/);
  const recovered = videoPage();
  recovered.data.flvUrl = "test.flv";
  recovered.onState({ detail: { code: 2007 } });
  context.mock.timers.tick(10000);
  recovered.onState({ detail: { code: 2004 } });
  context.mock.timers.tick(20000);
  assert.equal(recovered.data.error, "");
});

test("unloading clears the video timeout without late page writes", (context) => {
  context.mock.timers.enable({ apis: ["setTimeout"] });
  const page = videoPage();
  page.data.flvUrl = "test.flv";
  page.startVideoWait();
  page.onUnload();
  page.setData = () => assert.fail("Unexpected late update");
  context.mock.timers.tick(30000);
  page.onState({ detail: { code: 2004 } });
  page.data.flvUrl = "test.flv";
  page.onPlayerError({ detail: { errCode: -1 } });
});

test("video URL request timeout releases loading and keeps selection available for retry", async (context) => {
  context.mock.timers.enable({ apis: ["setTimeout"] });
  const page = videoPage();
  const pending = page.refresh();
  assert.equal(page.data.loading, true);
  context.mock.timers.tick(15000);
  await pending;
  assert.equal(page.data.loading, false);
  assert.equal(page.data.camera, "camera");
  assert.equal(page.data.flvUrl, "");
  assert.match(page.data.error, /超时/);
  assert.equal(aborts, 1);
});

test("native login timeout clears the timer and synchronous login failure permits retry", async (context) => {
  context.mock.timers.enable({ apis: ["setTimeout"] });
  app = { ...application, globalData: { ...application.globalData } };
  wx.login = (options) => options.fail({ errMsg: "login:fail timeout secret" });
  await assert.rejects(app.login(), (error) => error.code === "TIMEOUT" && !error.message.includes("secret"));
  assert.equal(app.loginPromise, null);
  wx.login = () => { throw new Error("secret"); };
  await assert.rejects(app.login(), /无法发起微信登录/);
  context.mock.timers.tick(20000);
  assert.equal(app.loginPromise, null);
  assert.equal(request, undefined);
});