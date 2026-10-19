const api = require("../../utils/api");

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
    wechatId: "",
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
    const camera = decodeURIComponent(query.camera || "");
    this.setData({ camera });
    wx.setNavigationBarTitle({ title: camera || "实时预览" });
    this.refresh();
  },

  onShow() {
    const { wechatId, feedPath, waterPath } = getApp().globalData;
    this.setData({
      wechatId,
      feedConfigured: !!feedPath && feedPath.startsWith("/") && !feedPath.startsWith("//"),
      waterConfigured: !!waterPath && waterPath.startsWith("/") && !waterPath.startsWith("//"),
    });
  },

  onUnload() {
    this.unloaded = true;
    clearTimeout(this.feedTimer);
    clearTimeout(this.waterTimer);
  },

  editWechatId() {
    getApp().editWechatId((wechatId) => this.setData({ wechatId }));
  },

  refresh() {
    if (this.data.loading) return;
    const { camera } = this.data;
    if (!camera) {
      this.setData({ error: "缺少 camera 参数" });
      return;
    }
    this.setData({ loading: true, error: "", flvUrl: "", playing: false });
    api
      .playUrl(camera)
      .then((data) => {
        if (this.unloaded) return;
        this.setData({ flvUrl: data.url || "", loading: false });
        if (!data.url) {
          this.setData({ error: "play 未返回 url" });
        }
      })
      .catch((e) => {
        if (this.unloaded) return;
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
    this.confirming = true;
    wx.showModal({
      title: `确认手动${options.label}`,
      content: `向设备 ${this.data.camera} 发送一次${options.label}指令？`,
      confirmText: `确认${options.label}`,
      confirmColor: options.color,
      success: (result) => {
        if (!result.confirm || this.unloaded) return;
        this.setData({ [options.pending]: true, [options.status]: `正在发送${options.label}指令` });
        api[action](this.data.camera)
          .then(() => {
            if (this.unloaded) return;
            this.setData({ [options.cooldown]: true, [options.status]: `${options.label}指令已提交` });
            wx.showToast({ title: "指令已提交", icon: "success" });
            this[options.timer] = setTimeout(() => {
              if (!this.unloaded) this.setData({ [options.cooldown]: false });
            }, 10000);
          })
          .catch(() => {
            if (this.unloaded) return;
            this.setData({ [options.status]: "未确认指令结果，请核实设备状态后再操作" });
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
