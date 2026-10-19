function headers() {
  const app = getApp();
  const h = { "content-type": "application/json" };
  if (app.globalData.apiToken) {
    h["X-Vigil-Token"] = app.globalData.apiToken;
  }
  return h;
}

function request(path) {
  const app = getApp();
  return new Promise((resolve, reject) => {
    wx.request({
      url: `${app.globalData.apiBase}${path}`,
      method: "GET",
      header: headers(),
      success(res) {
        if (res.statusCode >= 200 && res.statusCode < 300) {
          resolve(res.data);
        } else {
          reject(new Error((res.data && res.data.error) || `HTTP ${res.statusCode}`));
        }
      },
      fail: reject,
    });
  });
}

function listStreams() {
  return request("/api/streams");
}

function playUrl(camera) {
  return request(`/api/streams/${encodeURIComponent(camera)}/play`);
}

module.exports = {
  listStreams,
  playUrl,
};
