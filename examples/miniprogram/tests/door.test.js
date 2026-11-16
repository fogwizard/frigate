const assert = require("node:assert/strict");
const { test, beforeEach } = require("node:test");
const api = require("../utils/api");
const { parseDoorCode } = require("../utils/door");

global.Page = () => {};
require("../pages/index/index");
let app;
let requests;
const qrToken = "door_challenge_123456789";
const code = JSON.stringify({ type: "petshop-door", version: 1, token: qrToken });

beforeEach(() => {
  requests = [];
  app = {
    globalData: { apiBase: "https://api.example.com", doorPath: "/api/doors/open", openid: "openid-a", sessionToken: "user-token" },
    async login() { return this.globalData.openid; },
  };
  global.getApp = () => app;
  global.wx = {
    request(options) { requests.push(options); },
    showToast() {},
  };
});

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