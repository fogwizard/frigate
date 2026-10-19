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

module.exports = {
  loginWithCode,
  listStreams,
  playUrl,
  feed,
  water,
};
