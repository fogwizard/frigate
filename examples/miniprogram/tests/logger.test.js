const assert = require("node:assert/strict");
const { test } = require("node:test");
const logger = require("../utils/logger");
const api = require("../utils/api");

test("logger drops secrets arbitrary strings nested objects and unrecognized fields", (context) => {
  const output = [];
  context.mock.method(console, "info", (...args) => output.push(args));
  logger.info("payment.result", { operation: "order", outcome: "pending", count: 2,
    openid: "secret-openid", token: "secret-token", qrToken: "secret-qr", paySign: "secret-sign",
    url: "https://private.example?token=secret", error: { message: "secret-error" },
    requestId: "secret-request", canControl: true, statusCode: "secret-status" });
  const serialized = JSON.stringify(output);
  assert.doesNotMatch(serialized, /secret|private/);
  assert.match(serialized, /payment.result/);
  assert.equal(output[0][1].count, 2);
  assert.equal(output[0][1].canControl, true);
});

test("request logs share an ID and never include URL body headers or response errors", async (context) => {
  const output = [];
  for (const level of ["info", "warn", "error"]) context.mock.method(console, level, (...args) => output.push(args));
  global.getApp = () => ({ globalData: { apiBase: "https://secret-host.example", sessionToken: "secret-token", role: "user", feedPath: "/secret-path" } });
  let request;
  global.wx = { request(options) { request = options; } };
  const pending = api.feed("secret-camera");
  request.success({ statusCode: 403, data: { error: "secret-error" } });
  await assert.rejects(pending, /secret-error/);
  assert.equal(output[0][1].requestId, output[1][1].requestId);
  assert.equal(output[1][1].statusCode, 403);
  assert.equal(output[1][1].operation, "feed");
  assert.doesNotMatch(JSON.stringify(output), /secret/);
});

test("network failure logs timing without the platform raw error", async (context) => {
  const output = [];
  for (const level of ["info", "warn", "error"]) context.mock.method(console, level, (...args) => output.push(args));
  global.getApp = () => ({ globalData: { apiBase: "https://secret-host.example", sessionToken: "secret-token" } });
  global.wx = { request(options) { options.fail({ errMsg: "request:fail secret-url" }); } };
  await assert.rejects(api.playUrl("secret-camera"), /secret-url/);
  const failure = output.find((entry) => entry[0].includes("request.network_failed"));
  assert.equal(failure[1].operation, "play");
  assert.equal(typeof failure[1].elapsedMs, "number");
  assert.doesNotMatch(JSON.stringify(output), /secret/);
});

test("player errors log only a numeric error code and not the raw detail", (context) => {
  const output = [];
  context.mock.method(console, "error", (...args) => output.push(args));
  let definition;
  global.Page = (page) => { definition = page; };
  require("../pages/live/live");
  const page = { ...definition, data: { ...definition.data }, setData(update) { Object.assign(this.data, update); } };
  page.onPlayerError({ detail: { errCode: -1, errMsg: "secret-player-url", url: "secret-stream" } });
  assert.equal(output[0][1].code, -1);
  assert.doesNotMatch(JSON.stringify(output), /secret/);
  assert.equal(page.data.playing, false);
});