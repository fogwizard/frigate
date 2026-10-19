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
  if (authenticated && !app.globalData.sessionToken) {
    return Promise.reject(new Error("请先完成微信登录"));
  }
  return new Promise((resolve, reject) => {
    wx.request({
      url: `${app.globalData.apiBase}${path}`,
      method: options.method || "GET",
      data: options.data,
      timeout: 15000,
      header: headers(authenticated),
      success(res) {
        if (res.statusCode >= 200 && res.statusCode < 300) {
          resolve(res.data);
        } else {
          if (authenticated && res.statusCode === 401) {
            app.globalData.sessionToken = "";
            app.globalData.openid = "";
          }
          const error = new Error(res.statusCode === 401
            ? "登录已失效，请刷新重新登录"
            : (res.data && res.data.error) || `HTTP ${res.statusCode}`);
          error.statusCode = res.statusCode;
          reject(error);
        }
      },
      fail(error) {
        reject(new Error(error.errMsg || "网络请求失败"));
      },
    });
  });
}

function loginWithCode(code) {
  const path = getApp().globalData.loginPath;
  if (typeof path !== "string" || !path.startsWith("/") || path.startsWith("//")) {
    return Promise.reject(new Error("尚未配置有效的登录接口"));
  }
  return request(path, { method: "POST", data: { code }, public: true });
}

function listStreams() {
  return request("/api/streams");
}

function boardingRequest(path, options) {
  const base = getApp().globalData.boardingPath;
  if (typeof base !== "string" || !base.startsWith("/") || base.startsWith("//") || base.endsWith("/")) {
    return Promise.reject(new Error("尚未配置有效的寄养接口"));
  }
  return request(`${base}${path}`, options);
}

function boardingCameras() {
  return boardingRequest("/records").then((data) => {
    if (!data || !Array.isArray(data.records)) throw new Error("寄养记录格式错误");
    return { cameras: data.records.filter((record) => record && record.status === "active" &&
      record.paymentStatus === "paid" && typeof record.id === "string" && record.id &&
      typeof record.camera === "string" && record.camera).map((record) => ({
      camera: record.camera, number: record.number, label: record.label,
    })) };
  });
}

function boardingCages(qrToken) {
  return boardingRequest("/cages", { method: "POST", data: { qrToken } });
}

function createBoardingOrder(qrToken, cageId, requestId) {
  return boardingRequest("/orders", { method: "POST", data: { qrToken, cageId, requestId } });
}

function boardingOrder(orderId) {
  return boardingRequest(`/orders/${encodeURIComponent(orderId)}`);
}

function playUrl(camera) {
  return request(`/api/streams/${encodeURIComponent(camera)}/play`);
}

function deviceAction(camera, path, label) {
  if (!path || !path.startsWith("/") || path.startsWith("//")) {
    return Promise.reject(new Error(`尚未配置有效的${label}接口`));
  }
  if (!camera) {
    return Promise.reject(new Error("请选择设备"));
  }
  return request(path, { method: "POST", data: { camera } }).then((data) => {
    if (data && data.success === false) {
      throw new Error(data.error || data.message || `${label}请求未被接受`);
    }
    return data;
  });
}

function feed(camera) {
  return deviceAction(camera, getApp().globalData.feedPath, "喂食");
}

function water(camera) {
  return deviceAction(camera, getApp().globalData.waterPath, "喂水");
}

function openDoor(qrToken) {
  const path = getApp().globalData.doorPath;
  if (typeof path !== "string" || !path.startsWith("/") || path.startsWith("//")) {
    return Promise.reject(new Error("尚未配置有效的开门接口"));
  }
  if (typeof qrToken !== "string" || !/^[A-Za-z0-9_-]{16,512}$/.test(qrToken)) {
    return Promise.reject(new Error("无效的门店二维码凭证"));
  }
  return request(path, { method: "POST", data: { qrToken } }).then((data) => {
    if (!data || data.success !== true || !["accepted", "opened"].includes(data.status)) {
      throw new Error((data && (data.error || data.message)) || "未确认开门结果，请核实门锁状态");
    }
    return data;
  });
}

module.exports = {
  boardingCameras,
  boardingCages,
  createBoardingOrder,
  boardingOrder,
  loginWithCode,
  listStreams,
  playUrl,
  feed,
  water,
  openDoor,
};
