const api = require("../../utils/api");
const { normalizeCameras } = require("../../utils/cameras");
const logger = require("../../utils/logger");

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
    isAdmin: false,
    canControl: false,
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
    this.stopVideoWait();
    clearTimeout(this.feedTimer);
    clearTimeout(this.waterTimer);
  },

  loadCameras(preferredCamera = this.data.camera) {
    if (this.unloaded || this.data.camerasLoading || this.confirming || this.data.feeding || this.data.watering) return;
    this.playRequest = (this.playRequest || 0) + 1;
    this.stopVideoWait();
    this.setData({ camerasLoading: true, cameras: [], camera: "", flvUrl: "", playing: false, loading: false, error: "", isAdmin: false, canControl: false });
    return getApp().login()
      .then(() => api.accessibleCameras())
      .then((data) => {
        if (this.unloaded) return;
        const cameras = normalizeCameras(data);
        const cameraIndex = Math.max(0, cameras.findIndex((item) => item.camera === preferredCamera));
        this.setData({ cameras, cameraIndex, isAdmin: data.isAdmin, camerasLoading: false, openid: getApp().globalData.openid });
        if (!cameras.length) {
          this.setData({ error: data.isAdmin ? "暂无可查看镜头，请联系门店配置" : "没有有效寄养记录，请让门店按手机号登记" });
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

  switchCamera(cameraIndex) {
    if (this.unloaded || this.confirming || this.data.camerasLoading || this.data.feeding || this.data.watering) return;
    if (!Number.isInteger(cameraIndex)) return;
    const selected = this.data.cameras[cameraIndex];
    if (!selected) return;
    const isAdmin = this.data.isAdmin || getApp().globalData.role === "admin";
    logger.info("video.camera_selected", { isAdmin, canControl: !isAdmin || selected.canControl === true });
    this.setData({ cameraIndex, camera: selected.camera, isAdmin, canControl: !isAdmin || selected.canControl === true, feedStatus: "", waterStatus: "" });
    wx.setNavigationBarTitle({ title: selected.label });
    return this.refresh(true);
  },

  clearAccess() {
    this.stopVideoWait();
    this.playRequest = (this.playRequest || 0) + 1;
    this.setData({ cameras: [], camera: "", flvUrl: "", playing: false, loading: false, canControl: false, isAdmin: getApp().globalData.role === "admin", openid: getApp().globalData.openid });
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
    this.stopVideoWait();
    this.videoTimedOut = false;
    // Clearing src destroys live-player and can fire a spurious binderror.
    this.ignoringPlayerError = true;
    this.setData({ loading: true, error: "", flvUrl: "", playing: false });
    return api
      .playUrl(camera)
      .then((data) => {
        if (this.unloaded || this.playRequest !== playRequest) return;
        const url = data.url || "";
        this.ignoringPlayerError = false;
        this.setData({ flvUrl: url, loading: false }, () => {
          if (this.unloaded || this.playRequest !== playRequest || !url) return;
          this.startLivePlayer();
        });
        if (!url) {
          logger.warn("video.url_missing", { operation: "play", outcome: "failed" });
          this.setData({ error: "play 未返回 url" });
        } else {
          this.startVideoWait();
        }
      })
      .catch((e) => {
        if (this.unloaded || this.playRequest !== playRequest) return;
        this.ignoringPlayerError = false;
        if (e.statusCode === 401 || e.statusCode === 403) this.clearAccess();
        this.setData({ error: String(e.message || e), loading: false });
      });
  },

  startLivePlayer() {
    if (this.unloaded || !this.data.flvUrl) return;
    try {
      const ctx = wx.createLivePlayerContext("player", this);
      ctx.stop({
        complete: () => {
          if (this.unloaded || !this.data.flvUrl) return;
          ctx.play({
            fail: (err) => {
              const code = Number(err && err.errCode);
              logger.error("video.player_failed", {
                code: Number.isFinite(code) ? code : undefined,
                operation: "play",
                outcome: "failed",
              });
            },
          });
        },
      });
    } catch (_) {
      // createLivePlayerContext is unavailable in DevTools; binderror covers the phone.
    }
  },

  stopVideoWait() {
    clearTimeout(this.videoTimer);
    this.videoTimer = null;
  },

  startVideoWait() {
    if (this.videoTimer || !this.data.flvUrl || this.unloaded) return;
    const playRequest = this.playRequest;
    this.videoTimer = setTimeout(() => {
      this.videoTimer = null;
      if (this.unloaded || this.playRequest !== playRequest || this.data.playing) return;
      this.videoTimedOut = true;
      logger.warn("video.timed_out", { operation: "play", outcome: "timeout" });
      this.setData({ flvUrl: "", playing: false, loading: false, error: "视频连接或缓冲超时，请刷新画面重试" });
    }, 20000);
  },

  onVideoPlay() {
    if (this.unloaded || this.videoTimedOut) return;
    this.stopVideoWait();
    if (!this.data.playing) logger.info("video.playing", { code: 2004 });
    this.setData({ playing: true, error: "" });
  },

  onState(e) {
    if (this.unloaded || this.videoTimedOut) return;
    if (e.detail.code === 2004) {
      this.stopVideoWait();
      if (!this.data.playing) logger.info("video.playing", { code: 2004 });
      this.setData({ playing: true, error: "" });
    } else if (e.detail.code === 2007) {
      if (this.data.playing) logger.warn("video.buffering", { code: 2007 });
      this.setData({ playing: false });
      this.startVideoWait();
    } else if (e.detail.code < 0 || e.detail.code === 2103) {
      this.stopVideoWait();
      logger.warn("video.interrupted", { code: e.detail.code });
      this.setData({ playing: false, error: "视频连接中断，请刷新重试" });
    }
  },

  onPlayerError(e) {
    if (this.unloaded || this.videoTimedOut || this.ignoringPlayerError || !this.data.flvUrl) return;
    this.stopVideoWait();
    const raw = e && e.detail ? e.detail.errCode : undefined;
    const code = typeof raw === "number" ? raw : Number(raw);
    const safeCode = Number.isFinite(code) ? code : undefined;
    logger.error("video.player_failed", { code: safeCode, operation: "play", outcome: "failed" });
    this.setData({
      playing: false,
      error: safeCode !== undefined ? `视频暂时无法播放（${safeCode}）` : "视频暂时无法播放，请用真机调试（开发者工具不支持 live-player）",
    });
  },

  manualFeed() {
    this.manualAction("feed");
  },

  manualWater() {
    this.manualAction("water");
  },

  startCooldown(options) {
    clearTimeout(this[options.timer]);
    this.setData({ [options.cooldown]: true });
    this[options.timer] = setTimeout(() => {
      if (!this.unloaded) this.setData({ [options.cooldown]: false });
    }, 10000);
  },

  manualAction(action) {
    const options = deviceActions[action];
    if (!options || this.unloaded) return;
    const selected = this.data.cameras.find((item) => item.camera === this.data.camera);
    if ((this.data.isAdmin || getApp().globalData.role === "admin") &&
      (!this.data.canControl || !selected || selected.canControl !== true)) {
      logger.warn("device.blocked", { operation: action, isAdmin: true, outcome: "denied" });
      return;
    }
    if (this.data[options.pending] || this.data[options.cooldown] || this.confirming) return;
    if (!this.data[options.configured] || !this.data.camera) return;
    const camera = this.data.camera;
    if (!selected) return;
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
        if (!result.confirm || this.unloaded || camera !== this.data.camera) {
          if (!result.confirm) logger.info("device.cancelled", { operation: action, outcome: "cancelled" });
          return;
        }
        this.setData({ [options.pending]: true, [options.status]: `正在发送${options.label}指令` });
        api[action](camera)
          .then(() => {
            if (this.unloaded) return;
            this.setData({ [options.status]: `${options.label}指令已提交` });
            wx.showToast({ title: "指令已提交", icon: "success" });
          })
          .catch((error) => {
            if (this.unloaded) return;
            logger.warn("device.result_uncertain", { operation: action, statusCode: error.statusCode, outcome: "unknown" });
            if (error.statusCode === 401 || error.statusCode === 403) this.clearAccess();
            this.setData({ [options.status]: "未确认指令结果，请核实设备状态后再操作" });
            wx.showToast({ title: "请核实设备状态", icon: "none" });
          })
          .finally(() => {
            if (!this.unloaded) {
              this.startCooldown(options);
              this.setData({ [options.pending]: false });
            }
          });
      },
      complete: () => {
        this.confirming = false;
      },
    });
  },
});
