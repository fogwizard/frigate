const api = require("../../utils/api");
const { normalizeCameras } = require("../../utils/cameras");
const logger = require("../../utils/logger");

Page({
  data: {
    cameras: [],
    cameraIndex: 0,
    loading: false,
    error: "",
    openid: "",
    phone: "",
    phoneDraft: "",
    contactBusy: false,
    loggedIn: false,
    loginBusy: false,
    isAdmin: false,
  },

  onShow() {
    const { openid, phone } = getApp().globalData;
    this.setData({
      openid,
      phone: phone || "",
      loggedIn: Boolean(openid),
    });
    if (openid && !this.data.loginBusy) this.reload();
  },

  onUnload() {
    this.unloaded = true;
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
        const app = getApp();
        const openid = app.globalData.openid;
        this.setData({ cameras, cameraIndex, isAdmin: data.isAdmin, loading: false, openid, phone: app.globalData.phone || "", loggedIn: Boolean(openid) });
      })
      .catch((e) => {
        if (this.unloaded) return;
        logger.warn("home.load_failed", { statusCode: e.statusCode });
        const app = getApp();
        const openid = app.globalData.openid;
        this.setData({ error: String(e.message || e), loading: false, cameras: [], openid, phone: app.globalData.phone || "", loggedIn: Boolean(openid) });
      });
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
        this.setData({
          openid,
          phone: app.globalData.phone || "",
          loggedIn: true,
          loginBusy: false,
        });
        return this.reload();
      })
      .catch((error) => {
        if (this.unloaded) return;
        this.setData({ loginBusy: false, loggedIn: false, openid: "", error: error.message || "微信登录失败" });
      });
  },

  onPhone(event) {
    this.setData({ phoneDraft: event.detail.value });
  },

  saveContact() {
    if (this.unloaded || this.data.contactBusy || !this.data.loggedIn || this.data.phone) return;
    const phone = this.data.phoneDraft.trim();
    if (!phone) {
      wx.showToast({ title: "请填写手机号", icon: "none" });
      return;
    }
    this.setData({ contactBusy: true, error: "" });
    api.saveProfile(phone)
      .then((saved) => {
        if (this.unloaded) return;
        const app = getApp();
        app.globalData.phone = saved.phone || "";
        this.setData({
          phone: app.globalData.phone,
          phoneDraft: "",
          contactBusy: false,
        });
        wx.showToast({ title: "已保存", icon: "success" });
        return this.reload();
      })
      .catch((error) => {
        if (this.unloaded) return;
        this.setData({ contactBusy: false, error: error.message || "保存失败" });
      });
  },

  openMyBoarding() {
    if (this.unloaded) return;
    if (!getApp().globalData.openid) return this.loginWithWechat();
    if (!this.data.loading) this.reload();
  },

  async openLive() {
    const selected = this.data.cameras[this.data.cameraIndex];
    if (this.data.loading || this.unloaded) return;
    if (!selected) {
      wx.showToast({ title: this.data.isAdmin ? "暂无可查看镜头" : "请让门店按手机号登记", icon: "none" });
      return;
    }
    this.setData({ loading: true });
    try {
      await getApp().login();
      const data = await api.accessibleCameras();
      if (this.unloaded) return;
      if (!data.cameras.some((item) => item.camera === selected.camera)) {
        this.setData({ cameras: [], isAdmin: data.isAdmin, error: data.isAdmin ? "当前镜头不可访问，请刷新列表" : "没有有效寄养记录，请让门店按手机号登记" });
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
