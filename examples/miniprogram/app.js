const api = require("./utils/api");
const logger = require("./utils/logger");

App({
  globalData: {
    // 换成你的 Vigil HTTPS API 根（不要末尾斜杠）
    apiBase: "https://api.example.com",
    loginPath: "/api/miniprogram/login",
    feedPath: "",
    waterPath: "",
    doorPath: "",
    boardingPath: "",
    openid: "",
    sessionToken: "",
    role: "user",
  },

  login() {
    if (this.globalData.sessionToken && this.globalData.openid) {
      return Promise.resolve(this.globalData.openid);
    }
    if (this.loginPromise) return this.loginPromise;
    logger.info("login.started", { operation: "login", outcome: "started" });
    this.loginPromise = new Promise((resolve, reject) => {
      wx.login({
        timeout: 15000,
        success(result) {
          if (!result.code) {
            reject(new Error("微信登录未返回凭证，请重试"));
            return;
          }
          resolve(result.code);
        },
        fail() { reject(new Error("微信登录失败，请重试")); },
      });
    }).then((code) => api.loginWithCode(code)).then((session) => {
      if (!session || typeof session.openid !== "string" || !session.openid.trim() ||
          typeof session.token !== "string" || !session.token.trim()) {
        throw new Error("登录接口未返回有效的 openid 和 token");
      }
      this.globalData.openid = session.openid;
      this.globalData.sessionToken = session.token;
      this.globalData.role = session.role === "admin" ? "admin" : "user";
      logger.info("login.completed", { operation: "login", outcome: "success", isAdmin: this.globalData.role === "admin" });
      return session.openid;
    }).catch((error) => {
      logger.error("login.failed", { operation: "login", outcome: "failed", statusCode: error.statusCode });
      throw error;
    }).finally(() => {
      this.loginPromise = null;
    });
    return this.loginPromise;
  },
});
