const logger = require("./logger");
let requestSequence = 0;

function headers(authenticated) {
  const app = getApp();
  const h = { "content-type": "application/json" };
  if (authenticated) {
    h.Authorization = `Bearer ${app.globalData.sessionToken}`;
  }
  return h;
}

function request(path, options = {}) {
  const app = getApp();
  const authenticated = !options.public;
  const operation = options.operation || "unknown";
  if (authenticated && !app.globalData.sessionToken) {
    logger.warn("request.blocked", { operation, outcome: "denied" });
    return Promise.reject(new Error("请先完成微信登录"));
  }
  const startedAt = Date.now();
  const requestId = `req-${startedAt}-${++requestSequence}`;
  const context = { requestId, operation };
  logger.info("request.started", { ...context, outcome: "started" });
  return new Promise((resolve, reject) => {
    let settled = false;
    let task;
    const finish = (callback, value) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      callback(value);
    };
    const timedOut = () => {
      if (settled) return;
      logger.warn("request.timed_out", { ...context, elapsedMs: Date.now() - startedAt, outcome: "timeout" });
      const error = new Error("请求超时，请检查网络；操作结果请先核实，勿重复提交");
      error.code = "TIMEOUT";
      finish(reject, error);
    };
    const timer = setTimeout(() => {
      timedOut();
      if (task && typeof task.abort === "function") task.abort();
    }, 15000);
    try {
      task = wx.request({
      url: `${app.globalData.apiBase}${path}`,
      method: options.method || "GET",
      data: options.data,
      timeout: 15000,
      header: headers(authenticated),
      success(res) {
        if (settled) return;
        const details = { ...context, statusCode: res.statusCode, elapsedMs: Date.now() - startedAt };
        if (res.statusCode >= 200 && res.statusCode < 300) {
          logger.info("request.completed", { ...details, outcome: "success" });
          finish(resolve, res.data);
        } else {
          logger.warn("request.completed", { ...details, outcome: res.statusCode === 403 ? "denied" : "failed" });
          if (authenticated && res.statusCode === 401) {
            app.globalData.sessionToken = "";
            app.globalData.openid = "";
            app.globalData.role = "user";
            logger.warn("session.expired", { ...context, statusCode: 401 });
          }
          const error = new Error(res.statusCode === 401
            ? "登录已失效，请刷新重新登录"
            : (res.data && res.data.error) || `HTTP ${res.statusCode}`);
          error.statusCode = res.statusCode;
          finish(reject, error);
        }
      },
      fail(error) {
        if (settled) return;
        if (/timeout|timed out/i.test(error.errMsg || "")) {
          timedOut();
          return;
        }
        logger.error("request.network_failed", { ...context, elapsedMs: Date.now() - startedAt, outcome: "failed" });
        const failure = new Error(error.errMsg || "网络请求失败");
        failure.code = "NETWORK_ERROR";
        finish(reject, failure);
      },
      });
    } catch (error) {
      logger.error("request.start_failed", { ...context, elapsedMs: Date.now() - startedAt, outcome: "failed" });
      finish(reject, new Error("无法发起请求，请稍后重试"));
    }
  });
}

function saveProfile(phone, wechatId) {
  return request("/api/miniprogram/profile", {
    method: "POST",
    data: { phone, wechatId },
    operation: "profile",
  });
}

function loginWithCode(code) {
  const path = getApp().globalData.loginPath;
  if (typeof path !== "string" || !path.startsWith("/") || path.startsWith("//")) {
    return Promise.reject(new Error("尚未配置有效的登录接口"));
  }
  return request(path, { method: "POST", data: { code }, public: true, operation: "login" });
}

function listStreams() {
  return request("/api/streams", { operation: "streams" });
}

function accessibleCameras() {
  if (getApp().globalData.role !== "admin") {
    return boardingCameras().then((data) => {
      logger.info("cameras.loaded", { isAdmin: false, count: data.cameras.length });
      return { ...data, isAdmin: false };
    });
  }
  return listStreams().then((data) => {
    if (!data || !Array.isArray(data.cameras)) throw new Error("镜头列表格式错误");
    logger.info("cameras.loaded", { isAdmin: true, count: data.cameras.length });
    return { isAdmin: true, cameras: data.cameras.map((item) => typeof item === "string"
      ? { camera: item, canControl: false }
      : item && { camera: item.camera, number: item.number, label: item.label, canControl: item.canControl === true }) };
  });
}

function boardingRequest(path, options) {
  const base = getApp().globalData.boardingPath;
  if (typeof base !== "string" || !base.startsWith("/") || base.startsWith("//") || base.endsWith("/")) {
    return Promise.reject(new Error("尚未配置有效的寄养接口"));
  }
  return request(`${base}${path}`, options);
}

function boardingCameras() {
  return boardingRequest("/records", { operation: "records" }).then((data) => {
    if (!data || !Array.isArray(data.records)) throw new Error("寄养记录格式错误");
    return { cameras: data.records.filter((record) => record && record.status === "active" &&
      record.paymentStatus === "paid" && typeof record.id === "string" && record.id &&
      typeof record.camera === "string" && record.camera).map((record) => ({
      camera: record.camera, number: record.number, label: record.label,
    })) };
  });
}

function boardingCages(qrToken) {
  return boardingRequest("/cages", { method: "POST", data: { qrToken }, operation: "cages" });
}

function createBoardingOrder(qrToken, cageId, requestId) {
  return boardingRequest("/orders", { method: "POST", data: { qrToken, cageId, requestId }, operation: "create_order" });
}

function boardingOrder(orderId) {
  return boardingRequest(`/orders/${encodeURIComponent(orderId)}`, { operation: "order" });
}

function playUrl(camera) {
  return request(`/api/streams/${encodeURIComponent(camera)}/play`, { operation: "play" });
}

function deviceAction(camera, path, label, operation) {
  if (!path || !path.startsWith("/") || path.startsWith("//")) {
    return Promise.reject(new Error(`尚未配置有效的${label}接口`));
  }
  if (!camera) {
    return Promise.reject(new Error("请选择设备"));
  }
  const submit = () => request(path, { method: "POST", data: { camera }, operation }).then((data) => {
    if (data && data.success === false) {
      logger.warn("device.rejected", { operation, outcome: "denied" });
      throw new Error(data.error || data.message || `${label}请求未被接受`);
    }
    logger.info("device.submitted", { operation, outcome: "accepted" });
    return data;
  });
  if (getApp().globalData.role === "admin") {
    return accessibleCameras().then((data) => {
      if (!data.isAdmin || !data.cameras.some((item) => item && item.camera === camera && item.canControl === true)) {
        logger.warn("device.permission_denied", { operation, isAdmin: true, outcome: "denied" });
        throw new Error(`管理员无此镜头的${label}权限`);
      }
      return submit();
    });
  }
  return submit();
}

function feed(camera) {
  return deviceAction(camera, getApp().globalData.feedPath, "喂食", "feed");
}

function water(camera) {
  return deviceAction(camera, getApp().globalData.waterPath, "喂水", "water");
}

function openDoor(qrToken) {
  const path = getApp().globalData.doorPath;
  if (typeof path !== "string" || !path.startsWith("/") || path.startsWith("//")) {
    return Promise.reject(new Error("尚未配置有效的开门接口"));
  }
  if (typeof qrToken !== "string" || !/^[A-Za-z0-9_-]{16,512}$/.test(qrToken)) {
    return Promise.reject(new Error("无效的门店二维码凭证"));
  }
  return request(path, { method: "POST", data: { qrToken }, operation: "door" }).then((data) => {
    if (!data || data.success !== true || !["accepted", "opened"].includes(data.status)) {
      throw new Error((data && (data.error || data.message)) || "未确认开门结果，请核实门锁状态");
    }
    logger.info("door.result", { outcome: data.status });
    return data;
  });
}

module.exports = {
  accessibleCameras,
  boardingCameras,
  boardingCages,
  createBoardingOrder,
  boardingOrder,
  loginWithCode,
  saveProfile,
  listStreams,
  playUrl,
  feed,
  water,
  openDoor,
};
