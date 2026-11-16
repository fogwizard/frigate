const api = require("../../utils/api");
const door = require("../../utils/door");
const { normalizeCameras } = require("../../utils/cameras");
const logger = require("../../utils/logger");

Page({
  data: {
    cameras: [],
    cameraIndex: 0,
    loading: false,
    error: "",
    openid: "",
    loggedIn: false,
    loginBusy: false,
    isAdmin: false,
    doorConfigured: false,
    doorBusy: false,
    doorCooldown: false,
    doorStatus: "",
  },

  onShow() {
    const { openid, doorPath } = getApp().globalData;
    this.setData({
      openid,
      loggedIn: Boolean(openid),
      doorConfigured: typeof doorPath === "string" && doorPath.startsWith("/") && !doorPath.startsWith("//"),
    });
    if (openid && !this.data.doorBusy && !this.data.loginBusy) this.reload();
  },

  onUnload() {
    this.unloaded = true;
    clearTimeout(this.doorTimer);
  },

  reload() {
    if (this.data.loading || this.unloaded) return;
    const selected = this.data.cameras[this.data.cameraIndex];
    this.setData({ loading: true, error: "", cameras: [], isAdmin: false });
    return getApp().login()
      .then(() => api.accessibleCameras())
      .then((data) => {
        if (this.unloaded) return;
        const cameras = normalizeCameras(data);
        const cameraIndex = selected ? Math.max(0, cameras.findIndex((item) => item.camera === selected.camera)) : 0;
        const openid = getApp().globalData.openid;
        this.setData({ cameras, cameraIndex, isAdmin: data.isAdmin, loading: false, openid, loggedIn: Boolean(openid) });
      })
      .catch((e) => {
        if (this.unloaded) return;
        logger.warn("home.load_failed", { statusCode: e.statusCode });
        const openid = getApp().globalData.openid;
        this.setData({ error: String(e.message || e), loading: false, cameras: [], openid, loggedIn: Boolean(openid) });
      });
  },

  async scanOpenDoor() {
    if (this.unloaded || this.data.doorBusy || this.data.doorCooldown) return;
    if (!this.data.doorConfigured) {
      wx.showToast({ title: "开门服务未接入", icon: "none" });
      return;
    }
    this.setData({ doorBusy: true, doorStatus: "" });
    logger.info("door.flow_started", { operation: "door", outcome: "started" });
    let submitted = false;
    try {
      const allowed = await door.confirm({
        title: "微信身份确认",
        content: "将使用微信登录身份申请门店开门权限，是否继续？",
        confirmText: "继续扫码",
      });
      if (!allowed || this.unloaded) return;
      await getApp().login();
      if (this.unloaded) return;
      this.setData({ openid: getApp().globalData.openid, doorStatus: "等待扫描门店二维码" });
      const qrToken = await door.scanDoorCode();
      if (this.unloaded) return;
      if (!qrToken) {
        logger.info("door.scan_cancelled", { outcome: "cancelled" });
        this.setData({ doorStatus: "已取消扫码" });
        return;
      }
      const confirmed = await door.confirm({
        title: "确认申请开门",
        content: "确认向此二维码对应的宠物店大门提交开门申请？",
        confirmText: "确认开门",
        confirmColor: "#167753",
      });
      if (this.unloaded) return;
      if (!confirmed) {
        logger.info("door.confirm_cancelled", { outcome: "cancelled" });
        this.setData({ doorStatus: "已取消开门" });
        return;
      }
      submitted = true;
      this.setData({ doorStatus: "正在验证开门权限" });
      const result = await api.openDoor(qrToken);
      if (this.unloaded) return;
      const message = result.status === "opened" ? "门锁已确认开门" : "开门申请已受理，等待门锁响应";
      this.setData({ doorStatus: message });
      wx.showToast({ title: result.status === "opened" ? "门锁已开" : "申请已受理", icon: "success" });
    } catch (error) {
      if (this.unloaded) return;
      logger.warn("door.flow_failed", { operation: "door", statusCode: error.statusCode, outcome: submitted ? "unknown" : "failed" });
      const denied = error.statusCode === 401 || error.statusCode === 403;
      const message = denied
        ? (error.statusCode === 401 ? "登录已失效，请重新扫码登录" : "没有此门店的开门权限")
        : (submitted ? "未确认开门结果，请核实门锁状态，勿连续重试" : error.message || "扫码失败，请重试");
      this.setData({ doorStatus: message, openid: getApp().globalData.openid });
      wx.showToast({ title: denied ? "开门申请被拒绝" : "开门未完成", icon: "none" });
    } finally {
      if (!this.unloaded) {
        this.setData({ doorBusy: false, doorCooldown: submitted });
        if (submitted) {
          this.doorTimer = setTimeout(() => {
            if (!this.unloaded) this.setData({ doorCooldown: false });
          }, 10000);
        }
      }
    }
  },

  selectCamera(event) {
    const cameraIndex = Number(event.detail.value);
    if (!this.data.loading && Number.isInteger(cameraIndex) && this.data.cameras[cameraIndex]) {
      this.setData({ cameraIndex });
    }
  },

  loginWithWechat() {
    if (this.unloaded || this.data.loginBusy) return;
    const app = getApp();
    app.globalData.sessionToken = "";
    app.globalData.openid = "";
    app.globalData.role = "user";
    this.setData({ loginBusy: true, error: "" });
    return app.login()
      .then((openid) => {
        if (this.unloaded) return;
        if (!openid) throw new Error("微信登录未返回 OpenID");
        this.setData({ openid, loggedIn: true, loginBusy: false });
        return this.reload();
      })
      .catch((error) => {
        if (this.unloaded) return;
        this.setData({ loginBusy: false, loggedIn: false, openid: "", error: error.message || "微信登录失败" });
      });
  },

  openMyBoarding() {
    if (this.data.doorBusy || this.unloaded) return;
    if (!getApp().globalData.openid) return this.loginWithWechat();
    if (!this.data.loading && !this.data.cameras.length) this.startBoarding();
  },

  startBoarding() {
    if (this.data.doorBusy || this.unloaded) return;
    wx.navigateTo({ url: "/pages/boarding/boarding" });
  },

  async openLive() {
    const selected = this.data.cameras[this.data.cameraIndex];
    if (this.data.loading || this.unloaded) return;
    if (!selected) {
      wx.showToast({ title: this.data.isAdmin ? "暂无可查看镜头" : "请先扫码寄养", icon: "none" });
      return;
    }
    this.setData({ loading: true });
    try {
      await getApp().login();
      const data = await api.accessibleCameras();
      if (this.unloaded) return;
      if (!data.cameras.some((item) => item.camera === selected.camera)) {
        this.setData({ cameras: [], isAdmin: data.isAdmin, error: data.isAdmin ? "当前镜头不可访问，请刷新列表" : "没有有效寄养记录，请扫码寄养" });
        return;
      }
      wx.navigateTo({ url: `/pages/live/live?camera=${encodeURIComponent(selected.camera)}` });
    } catch (error) {
      if (!this.unloaded) this.setData({ cameras: [], error: error.message });
    } finally {
      if (!this.unloaded) this.setData({ loading: false });
    }
  },
});
