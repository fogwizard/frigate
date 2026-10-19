const api = require("../../utils/api");
const { normalizeCameras } = require("../../utils/cameras");

const deviceActions = {
  feed: {
    label: "喂食",
    configured: "feedConfigured",
    pending: "feeding",
    cooldown: "cooldown",
    status: "feedStatus",
    timer: "feedTimer",
    color: "#167753",
  },
  water: {
    label: "喂水",
    configured: "waterConfigured",
    pending: "watering",
    cooldown: "waterCooldown",
    status: "waterStatus",
    timer: "waterTimer",
    color: "#256f9b",
  },
};

Page({
  data: {
    camera: "",
    flvUrl: "",
    loading: false,
    error: "",
    openid: "",
    cameras: [],
    cameraIndex: 0,
    camerasLoading: false,
    feedConfigured: false,
    feeding: false,
    cooldown: false,
    feedStatus: "",
    waterConfigured: false,
    watering: false,
    waterCooldown: false,
    waterStatus: "",
    playing: false,
  },

  onLoad(query) {
    return this.loadCameras(query.camera || "");
  },

  onShow() {
    const { openid, feedPath, waterPath } = getApp().globalData;
    this.setData({
      openid,
      feedConfigured: !!feedPath && feedPath.startsWith("/") && !feedPath.startsWith("//"),
      waterConfigured: !!waterPath && waterPath.startsWith("/") && !waterPath.startsWith("//"),
    });
  },

  onUnload() {
    this.unloaded = true;
    clearTimeout(this.feedTimer);
    clearTimeout(this.waterTimer);
  },

  loadCameras(preferredCamera = this.data.camera) {
    if (this.unloaded || this.data.camerasLoading || this.confirming || this.data.feeding || this.data.watering) return;
    this.playRequest = (this.playRequest || 0) + 1;
    this.setData({ camerasLoading: true, cameras: [], camera: "", flvUrl: "", playing: false, loading: false, error: "" });
    return getApp().login()
      .then(() => api.boardingCameras())
      .then((data) => {
        if (this.unloaded) return;
        const cameras = normalizeCameras(data);
        const cameraIndex = Math.max(0, cameras.findIndex((item) => item.camera === preferredCamera));
        this.setData({ cameras, cameraIndex, camerasLoading: false, openid: getApp().globalData.openid });
        if (!cameras.length) {
          this.setData({ error: "没有有效寄养记录，请扫码寄养" });
          return;
        }
        return this.switchCamera(cameraIndex);
      })
      .catch((error) => {
        if (!this.unloaded) this.setData({ camerasLoading: false, error: error.message, openid: getApp().globalData.openid });
      });
  },

  selectCamera(event) {
    return this.switchCamera(Number(event.detail.value));
  },

  startBoarding() {
    if (!this.unloaded && !this.confirming && !this.data.feeding && !this.data.watering) {
      wx.redirectTo({ url: "/pages/boarding/boarding" });
    }
  },

  switchCamera(cameraIndex) {
    if (this.unloaded || this.confirming || this.data.camerasLoading || this.data.feeding || this.data.watering) return;
    if (!Number.isInteger(cameraIndex)) return;
    const selected = this.data.cameras[cameraIndex];
    if (!selected) return;
    this.setData({ cameraIndex, camera: selected.camera, feedStatus: "", waterStatus: "" });
    wx.setNavigationBarTitle({ title: selected.label });
    return this.refresh(true);
  },

  clearAccess() {
    this.playRequest = (this.playRequest || 0) + 1;
    this.setData({ cameras: [], camera: "", flvUrl: "", playing: false, loading: false, openid: getApp().globalData.openid });
  },

  refresh(force = false) {
    if (this.unloaded || this.data.camerasLoading || this.confirming || this.data.feeding || this.data.watering) return;
    if (!getApp().globalData.sessionToken || !this.data.cameras.length) return this.loadCameras();
    if (this.data.loading && force !== true) return;
    const { camera } = this.data;
    if (!this.data.cameras.some((item) => item.camera === camera)) {
      this.clearAccess();
      this.setData({ error: "无权访问此镜头" });
      return;
    }
    const playRequest = (this.playRequest || 0) + 1;
    this.playRequest = playRequest;
    this.setData({ loading: true, error: "", flvUrl: "", playing: false });
    return api
      .playUrl(camera)
      .then((data) => {
        if (this.unloaded || this.playRequest !== playRequest) return;
        this.setData({ flvUrl: data.url || "", loading: false });
        if (!data.url) {
          this.setData({ error: "play 未返回 url" });
        }
      })
      .catch((e) => {
        if (this.unloaded || this.playRequest !== playRequest) return;
        if (e.statusCode === 401 || e.statusCode === 403) this.clearAccess();
        this.setData({ error: String(e.message || e), loading: false });
      });
  },

  onState(e) {
    if (e.detail.code === 2004) {
      this.setData({ playing: true, error: "" });
    } else if (e.detail.code === 2007) {
      this.setData({ playing: false });
    } else if (e.detail.code < 0 || e.detail.code === 2103) {
      this.setData({ playing: false, error: "视频连接中断，请刷新重试" });
    }
  },

  onPlayerError(e) {
    console.error("live-player error", e.detail);
    this.setData({ playing: false, error: "视频暂时无法播放，请刷新重试" });
  },

  manualFeed() {
    this.manualAction("feed");
  },

  manualWater() {
    this.manualAction("water");
  },

  manualAction(action) {
    const options = deviceActions[action];
    if (!options || this.unloaded) return;
    if (this.data[options.pending] || this.data[options.cooldown] || this.confirming) return;
    if (!this.data[options.configured] || !this.data.camera) return;
    const camera = this.data.camera;
    if (!this.data.cameras.some((item) => item.camera === camera)) return;
    if (!getApp().globalData.sessionToken) {
      this.clearAccess();
      this.setData({ error: "登录已失效，请刷新重新登录" });
      return;
    }
    this.confirming = true;
    wx.showModal({
      title: `确认手动${options.label}`,
      content: `向设备 ${this.data.camera} 发送一次${options.label}指令？`,
      confirmText: `确认${options.label}`,
      confirmColor: options.color,
      success: (result) => {
        if (!result.confirm || this.unloaded || camera !== this.data.camera) return;
        this.setData({ [options.pending]: true, [options.status]: `正在发送${options.label}指令` });
        api[action](camera)
          .then(() => {
            if (this.unloaded) return;
            this.setData({ [options.cooldown]: true, [options.status]: `${options.label}指令已提交` });
            wx.showToast({ title: "指令已提交", icon: "success" });
            this[options.timer] = setTimeout(() => {
              if (!this.unloaded) this.setData({ [options.cooldown]: false });
            }, 10000);
          })
          .catch((error) => {
            if (this.unloaded) return;
            if (error.statusCode === 401 || error.statusCode === 403) this.clearAccess();
            this.setData({ [options.status]: "未确认指令结果，请核实设备状态后再操作", [options.cooldown]: true });
            this[options.timer] = setTimeout(() => {
              if (!this.unloaded) this.setData({ [options.cooldown]: false });
            }, 10000);
            wx.showToast({ title: "请核实设备状态", icon: "none" });
          })
          .finally(() => {
            if (!this.unloaded) this.setData({ [options.pending]: false });
          });
      },
      complete: () => {
        this.confirming = false;
      },
    });
  },
});
