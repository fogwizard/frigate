const assert = require("node:assert/strict");
const { test, beforeEach } = require("node:test");
const api = require("../utils/api");
const boarding = require("../utils/boarding");
let requests;
let app;
beforeEach(() => {
  requests = [];
  app = { globalData: { boardingPath: "/api/boarding", apiBase: "https://api.example.com", sessionToken: "user-token" } };
  global.getApp = () => app;
  global.wx = { request(options) { requests.push(options); } };
});

test("boarding QR is distinct from door QR and rejects URLs", () => {
  assert.equal(boarding.parseBoardingCode(JSON.stringify({ type: "petshop-boarding", version: 1, token: "boarding_token_123456" })), "boarding_token_123456");
  for (const result of ["https://other.example", "null", JSON.stringify({ type: "petshop-door", version: 1, token: "boarding_token_123456" })]) {
    assert.throws(() => boarding.parseBoardingCode(result), /寄养二维码/);
  }
});

test("only available cages with a valid server price can be selected", () => {
  const cages = boarding.normalizeCages({ storeName: "门店", cages: [
    { id: "a", number: "01", amount: 1200, available: true },
    { id: "b", number: "02", amount: 1000, available: false },
    { id: "c", number: "03", amount: -1, available: true },
  ] });
  assert.equal(cages.length, 1);
  assert.equal(cages[0].label, "01号 · ¥12.00");
});

test("payment parameters are validated and only whitelisted fields passed to WeChat", () => {
  assert.throws(() => boarding.paymentParameters({ payment: {} }), /支付参数/);
  const payment = { timeStamp: "123", nonceStr: "nonce", package: "prepay_id=abc", signType: "RSA", paySign: "signature", success: "untrusted" };
  assert.equal(boarding.paymentParameters({ payment }).success, undefined);
});

test("only paid active boarding records grant camera access", async () => {
  const pending = api.boardingCameras();
  assert.equal(requests[0].header.Authorization, "Bearer user-token");
  requests[0].success({ statusCode: 200, data: { records: [
    { id: "paid", status: "active", paymentStatus: "paid", camera: "allowed", number: "01" },
    { id: "unpaid", status: "active", paymentStatus: "pending", camera: "blocked" },
    { id: "ended", status: "completed", paymentStatus: "paid", camera: "ended" },
  ] } });
  assert.deepEqual((await pending).cameras.map((camera) => camera.camera), ["allowed"]);
});

test("boarding configuration is required and create order never sends a client price", async () => {
  app.globalData.boardingPath = "";
  await assert.rejects(api.boardingCameras(), /寄养接口/);
  app.globalData.boardingPath = "/api/boarding";
  const pending = api.createBoardingOrder("qr-token", "cage-1", "attempt-1");
  assert.deepEqual(requests[0].data, { qrToken: "qr-token", cageId: "cage-1", requestId: "attempt-1" });
  requests[0].success({ statusCode: 200, data: { orderId: "order-1" } });
  await pending;
});