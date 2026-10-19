const assert = require("node:assert/strict");
const { test, beforeEach } = require("node:test");
let definition;
global.Page = (page) => { definition = page; };
require("../pages/boarding/boarding");
let app;
let page;
let requests;
let storage;
let navigations;
let payments;
const payment = { timeStamp: "123", nonceStr: "nonce", package: "prepay_id=test", signType: "RSA", paySign: "sign" };
const flush = () => new Promise(setImmediate);

beforeEach(async () => {
  app = { globalData: { openid: "user-a", sessionToken: "token", boardingPath: "/api/boarding", apiBase: "https://api.example.com" }, async login() {} };
  requests = [];
  storage = new Map();
  navigations = [];
  payments = 0;
  global.getApp = () => app;
  global.wx = {
    request(options) { requests.push(options); },
    getStorageSync(key) { return storage.get(key); },
    setStorageSync(key, value) { storage.set(key, value); },
    removeStorageSync(key) { storage.delete(key); },
    showModal(options) { options.success({ confirm: true }); },
    requestPayment(options) { payments += 1; options.success({}); },
    redirectTo(options) { navigations.push(options.url); },
  };
  page = { ...definition, data: { ...definition.data }, setData(update) { Object.assign(this.data, update); } };
  await page.onLoad();
  page.qrToken = "boarding_token_123456";
  page.data.cages = [{ id: "cage-1", number: "01", label: "01号 · ¥12.00", amount: 1200 }];
  page.data.storeName = "门店";
});

async function beginPayment() {
  const pending = page.payBoarding();
  await flush();
  requests[0].success({ statusCode: 200, data: { orderId: "order-1", paymentStatus: "pending", status: "pending", amount: 1200, payment } });
  await flush();
  return { pending };
}

test("payment success alone never grants access while server payment is pending", async () => {
  const { pending } = await beginPayment();
  assert.equal(payments, 1);
  assert.equal(navigations.length, 0);
  requests[1].success({ statusCode: 200, data: { paymentStatus: "pending", status: "pending" } });
  await pending;
  assert.equal(navigations.length, 0);
  assert.equal(page.data.pending, true);
  assert.equal(page.data.canPay, true);
  assert.equal(storage.get("boarding-order:user-a").orderId, "order-1");
});

test("server paid active order plus active boarding record opens camera", async () => {
  const { pending } = await beginPayment();
  requests[1].success({ statusCode: 200, data: { paymentStatus: "paid", status: "active", recordId: "record-1", camera: "camera-1" } });
  await flush();
  assert.match(requests[2].url, /\/records$/);
  requests[2].success({ statusCode: 200, data: { records: [{ id: "record-1", paymentStatus: "paid", status: "active", camera: "camera-1" }] } });
  await pending;
  assert.deepEqual(navigations, ["/pages/live/live?camera=camera-1"]);
  assert.equal(storage.has("boarding-order:user-a"), false);
  assert.equal(page.data.pending, false);
});

test("paid order without a matching active record waits for permissions", async () => {
  const { pending } = await beginPayment();
  requests[1].success({ statusCode: 200, data: { paymentStatus: "paid", status: "active", recordId: "record-1", camera: "camera-1" } });
  await flush();
  requests[2].success({ statusCode: 200, data: { records: [] } });
  await pending;
  assert.equal(navigations.length, 0);
  assert.match(page.data.status, /尚未同步/);
  assert.equal(page.data.pending, true);
});

test("payment cancellation retains the order without automatic payment retries", async () => {
  wx.requestPayment = (options) => { payments += 1; options.fail({ errMsg: "requestPayment:fail cancel" }); };
  const { pending } = await beginPayment();
  await pending;
  assert.equal(payments, 1);
  assert.equal(requests.length, 1);
  assert.equal(page.data.pending, true);
  assert.equal(page.data.canPay, false);
  assert.match(page.data.status, /勿重复付款/);
  await page.payBoarding();
  assert.equal(requests.length, 1);
});

test("duplicate taps cannot create a second order", async () => {
  const pending = page.payBoarding();
  await page.payBoarding();
  await flush();
  assert.equal(requests.length, 1);
  assert.ok(requests[0].data.requestId);
  requests[0].fail({ errMsg: "request:fail timeout" });
  await pending;
  assert.equal(storage.has("boarding-order:user-a"), true);
  assert.equal(navigations.length, 0);
});

test("uncertain create recovers using the same requestId instead of creating a new attempt", async () => {
  const pending = page.payBoarding();
  await flush();
  const requestId = requests[0].data.requestId;
  requests[0].fail({ errMsg: "request:fail timeout" });
  await pending;
  const recovery = page.checkPayment();
  await flush();
  assert.equal(requests[1].data.requestId, requestId);
  requests[1].success({ statusCode: 200, data: { orderId: "order-1" } });
  await flush();
  requests[2].success({ statusCode: 200, data: { paymentStatus: "pending", status: "pending" } });
  await recovery;
  assert.equal(payments, 0);
  assert.equal(page.data.canPay, true);
});

test("pending orders restore per OpenID and block rescanning", async () => {
  storage.set("boarding-order:user-a", { orderId: "order-1" });
  await page.onLoad();
  assert.equal(page.data.pending, true);
  wx.scanCode = () => assert.fail("Unexpected scan");
  await page.scanBoarding();
  assert.equal(requests.length, 0);
});

test("scan returns available cages for a validated boarding QR", async () => {
  wx.scanCode = (options) => options.success({ result: JSON.stringify({ type: "petshop-boarding", version: 1, token: "boarding_token_123456" }) });
  const pending = page.scanBoarding();
  await flush();
  assert.match(requests[0].url, /\/cages$/);
  requests[0].success({ statusCode: 200, data: { storeName: "门店 A", cages: [{ id: "cage-a", number: "08", amount: 2000, available: true }] } });
  await pending;
  assert.equal(page.data.cages[0].number, "08");
  assert.equal(page.data.storeName, "门店 A");
});

test("leaving before payment preparation never starts WeChat payment", async () => {
  const pending = page.payBoarding();
  await flush();
  page.onUnload();
  requests[0].success({ statusCode: 200, data: { orderId: "order-1", paymentStatus: "pending", payment } });
  await pending;
  assert.equal(payments, 0);
  assert.equal(navigations.length, 0);
  assert.equal(storage.get("boarding-order:user-a").orderId, "order-1");
});

test("invalid server order amount never launches payment", async () => {
  const pending = page.payBoarding();
  await flush();
  requests[0].success({ statusCode: 200, data: { orderId: "order-1", paymentStatus: "pending", status: "pending", amount: -1, payment } });
  await pending;
  assert.equal(payments, 0);
  assert.match(page.data.status, /订单金额无效/);
});

for (const status of ["closed", "cancelled", "refunded"]) {
  test(`${status} order clears pending state without payment or navigation`, async () => {
    page.savePending({ orderId: "order-1" });
    const pending = page.checkPayment();
    await flush();
    requests[0].success({ statusCode: 200, data: { status, paymentStatus: status === "refunded" ? "refunded" : "pending" } });
    await pending;
    assert.equal(page.data.pending, false);
    assert.equal(page.data.canPay, false);
    assert.equal(page.pendingOrder, null);
    assert.equal(storage.has("boarding-order:user-a"), false);
    assert.equal(payments, 0);
    assert.equal(navigations.length, 0);
  });
}

test("declining initial confirmation creates no order and starts no payment", async () => {
  wx.showModal = (options) => options.success({ confirm: false });
  await page.payBoarding();
  assert.equal(requests.length, 0);
  assert.equal(storage.size, 0);
  assert.equal(payments, 0);
  assert.equal(page.data.busy, false);
});

test("declining final amount confirmation preserves order without charging", async () => {
  wx.showModal = (options) => options.success({ confirm: options.title !== "确认订单金额" });
  const { pending } = await beginPayment();
  await pending;
  assert.equal(payments, 0);
  assert.equal(requests.length, 1);
  assert.equal(page.data.pending, true);
  assert.equal(page.data.busy, false);
  assert.equal(storage.get("boarding-order:user-a").orderId, "order-1");
});

test("cage inventory conflict preserves the attempt and cannot grant access", async () => {
  const pending = page.payBoarding();
  await flush();
  const requestId = requests[0].data.requestId;
  requests[0].success({ statusCode: 409, data: { error: "笼位已占用" } });
  await pending;
  assert.match(page.data.status, /笼位已占用/);
  assert.equal(storage.get("boarding-order:user-a").requestId, requestId);
  assert.equal(page.data.pending, true);
  assert.equal(page.data.busy, false);
  assert.equal(payments, 0);
  assert.equal(navigations.length, 0);
});

test("payment callback followed by query timeout retains the order for manual recovery", async () => {
  const { pending } = await beginPayment();
  requests[1].fail({ errMsg: "request:fail timeout" });
  await pending;
  assert.equal(payments, 1);
  assert.equal(requests.length, 2);
  assert.equal(page.data.pending, true);
  assert.equal(page.data.canPay, false);
  assert.equal(page.data.busy, false);
  assert.match(page.data.status, /勿重复付款/);
  assert.equal(navigations.length, 0);
});

test("paid but inactive order waits without querying records or reopening payment", async () => {
  page.savePending({ orderId: "order-1" });
  const pending = page.checkPayment();
  await flush();
  requests[0].success({ statusCode: 200, data: { paymentStatus: "paid", status: "processing" } });
  await pending;
  assert.equal(page.data.pending, true);
  assert.equal(page.data.canPay, false);
  assert.match(page.data.status, /等待寄养生效/);
  assert.equal(requests.length, 1);
  assert.equal(payments, 0);
  assert.equal(navigations.length, 0);
});

test("another user's stored pending order is not restored", async () => {
  storage.set("boarding-order:user-b", { orderId: "other-order" });
  await page.onLoad();
  assert.equal(page.data.pending, false);
  assert.equal(page.pendingOrder, null);
  assert.equal(storage.get("boarding-order:user-b").orderId, "other-order");
  assert.equal(requests.length, 0);
});

test("expired session while querying payment preserves recovery state", async () => {
  page.savePending({ orderId: "order-1" });
  const pending = page.checkPayment();
  await flush();
  requests[0].success({ statusCode: 401, data: {} });
  await pending;
  assert.equal(app.globalData.sessionToken, "");
  assert.equal(app.globalData.openid, "");
  assert.equal(page.data.pending, true);
  assert.equal(storage.get("boarding-order:user-a").orderId, "order-1");
  assert.equal(page.data.busy, false);
  assert.equal(requests.length, 1);
  assert.equal(payments, 0);
  assert.equal(navigations.length, 0);
});

test("scan cancellation is recoverable and never queries cages", async () => {
  wx.scanCode = (options) => options.fail({ errMsg: "scanCode:fail cancel" });
  await page.scanBoarding();
  assert.equal(page.data.busy, false);
  assert.equal(page.data.cages.length, 0);
  assert.equal(requests.length, 0);
  assert.equal(payments, 0);
});

test("camera permission denial reports an error and never creates an order", async () => {
  wx.scanCode = (options) => options.fail({ errMsg: "scanCode:fail auth deny" });
  await page.scanBoarding();
  assert.match(page.data.status, /相机权限/);
  assert.equal(page.data.busy, false);
  assert.equal(requests.length, 0);
  assert.equal(storage.size, 0);
});

test("late record response after unload never updates or navigates", async () => {
  page.savePending({ orderId: "order-1" });
  const pending = page.checkPayment();
  await flush();
  requests[0].success({ statusCode: 200, data: { paymentStatus: "paid", status: "active", recordId: "record-1", camera: "camera-1" } });
  await flush();
  page.onUnload();
  page.setData = () => assert.fail("Unexpected update after unload");
  requests[1].success({ statusCode: 200, data: { records: [{ id: "record-1", paymentStatus: "paid", status: "active", camera: "camera-1" }] } });
  await pending;
  assert.equal(navigations.length, 0);
  assert.equal(storage.get("boarding-order:user-a").orderId, "order-1");
});