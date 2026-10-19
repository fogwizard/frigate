function parseDoorCode(result) {
  if (typeof result !== "string" || result.length > 2048) {
    throw new Error("请扫描有效的门店开门二维码");
  }
  let data;
  try {
    data = JSON.parse(result);
  } catch (error) {
    throw new Error("请扫描有效的门店开门二维码");
  }
  if (!data || data.type !== "petshop-door" || data.version !== 1 ||
      typeof data.token !== "string" || !/^[A-Za-z0-9_-]{16,512}$/.test(data.token)) {
    throw new Error("请扫描有效的门店开门二维码");
  }
  return data.token;
}

function confirm(options) {
  return new Promise((resolve, reject) => {
    wx.showModal({ ...options, success: (result) => resolve(!!result.confirm), fail: reject });
  });
}

function scanDoorCode() {
  return new Promise((resolve, reject) => {
    wx.scanCode({
      onlyFromCamera: true,
      scanType: ["qrCode"],
      success(result) {
        try {
          resolve(parseDoorCode(result.result));
        } catch (error) {
          reject(error);
        }
      },
      fail(error) {
        if (/cancel/i.test(error.errMsg || "")) {
          resolve(null);
        } else {
          reject(new Error("无法扫码，请检查微信相机权限后重试"));
        }
      },
    });
  });
}

module.exports = { parseDoorCode, confirm, scanDoorCode };