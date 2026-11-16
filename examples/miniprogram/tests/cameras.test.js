const assert = require("node:assert/strict");
const { test, beforeEach } = require("node:test");

let indexDefinition;
global.Page = (definition) => { indexDefinition = definition; };
require("../pages/index/index");
let liveDefinition;
global.Page = (definition) => { liveDefinition = definition; };
require("../pages/live/live");
let app;
let requests;

beforeEach((context) => {
  context.mock.timers.enable({ apis: ["setTimeout"] });
  requests = [];
  app = { globalData: { openid: "user-a", sessionToken: "token-a", apiBase: "https://api.example.com", boardingPath: "/api/boarding" }, async login() {} };
  global.getApp = () => app;
  global.wx = { request(options) { requests.push(options); }, setNavigationBarTitle() {}, showToast() {} };
});

function createPage(definition) {
  return { ...definition, data: { ...definition.data }, setData(update) { Object.assign(this.data, update); } };
}

test("homepage normalizes 64 authorized cameras and opens the selected number", async () => {
  const page = createPage(indexDefinition);
  const pending = page.reload();
  await new Promise(setImmediate);
  const records = Array.from({ length: 70 }, (_, index) => ({ id: `record-${index}`, status: "active", paymentStatus: "paid", camera: `camera-${index}`, number: index + 1, label: `寄养 ${index + 1}` }));
  requests[0].success({ statusCode: 200, data: { records } });
  await pending;
  assert.equal(page.data.cameras.length, 64);
  page.selectCamera({ detail: { value: "63" } });
  let opened;
  wx.navigateTo = (options) => { opened = options.url; };
  const opening = page.openLive();
  await new Promise(setImmediate);
  requests[1].success({ statusCode: 200, data: { records } });
  await opening;
  assert.equal(opened, "/pages/live/live?camera=camera-63");
  page.selectCamera({ detail: { value: "64" } });
  assert.equal(page.data.cameraIndex, 63);
});

test("homepage rejects malformed lists and never opens an unselected camera", async () => {
  const page = createPage(indexDefinition);
  const pending = page.reload();
  await new Promise(setImmediate);
  requests[0].success({ statusCode: 200, data: {} });
  await pending;
  assert.match(page.data.error, /格式错误/);
  wx.navigateTo = () => assert.fail("Unexpected navigation");
  await page.openLive();
});

test("live entry loads permissions before video and ignores unassigned route cameras", async () => {
  const page = createPage(liveDefinition);
  const pending = page.onLoad({ camera: "private-camera" });
  await new Promise(setImmediate);
  assert.match(requests[0].url, /\/api\/boarding\/records$/);
  requests[0].success({ statusCode: 200, data: { records: [{ id: "record-1", status: "active", paymentStatus: "paid", camera: "allowed-camera" }] } });
  await new Promise(setImmediate);
  assert.match(requests[1].url, /allowed-camera\/play$/);
  requests[1].success({ statusCode: 200, data: { url: "allowed.flv" } });
  await pending;
  assert.equal(page.data.camera, "allowed-camera");
  assert.equal(page.data.openid, "user-a");
});

test("empty permissions never request video", async () => {
  const page = createPage(liveDefinition);
  const pending = page.loadCameras();
  await new Promise(setImmediate);
  requests[0].success({ statusCode: 200, data: { records: [] } });
  await pending;
  assert.equal(requests.length, 1);
  assert.equal(page.data.camera, "");
  assert.match(page.data.error, /手机号登记/);
});

test("fast camera switching ignores stale video responses", async () => {
  const page = createPage(liveDefinition);
  page.data.cameras = [{ camera: "first", label: "01号" }, { camera: "second", label: "02号" }];
  const first = page.switchCamera(0);
  const second = page.selectCamera({ detail: { value: "1" } });
  requests[1].success({ statusCode: 200, data: { url: "second.flv" } });
  await second;
  requests[0].success({ statusCode: 200, data: { url: "first.flv" } });
  await first;
  assert.equal(page.data.flvUrl, "second.flv");
  assert.equal(page.data.camera, "second");
});

test("commands lock the selected camera during confirmation and submission", async () => {
  const page = createPage(liveDefinition);
  page.data.cameras = [{ camera: "first", label: "01号" }, { camera: "second", label: "02号" }];
  page.data.camera = "first";
  page.data.feedConfigured = true;
  app.globalData.feedPath = "/feed";
  let modal;
  wx.showModal = (options) => { modal = options; };
  wx.showToast = () => {};
  page.manualFeed();
  page.switchCamera(1);
  assert.equal(page.data.camera, "first");
  modal.success({ confirm: true });
  modal.complete();
  page.switchCamera(1);
  assert.equal(page.data.camera, "first");
  assert.deepEqual(requests[0].data, { camera: "first" });
  page.onUnload();
  requests[0].success({ statusCode: 200, data: {} });
  await new Promise(setImmediate);
});

test("forbidden playback clears access and blocks device commands", async () => {
  const page = createPage(liveDefinition);
  page.data.cameras = [{ camera: "first", label: "01号" }];
  const pending = page.switchCamera(0);
  requests[0].success({ statusCode: 403, data: { error: "Forbidden" } });
  await pending;
  assert.equal(page.data.camera, "");
  assert.equal(page.data.flvUrl, "");
  assert.deepEqual(page.data.cameras, []);
  page.manualFeed();
  assert.equal(requests.length, 1);
});