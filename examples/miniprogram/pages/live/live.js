const api = require("../../utils/api");

Page({
  data: {
    camera: "",
    flvUrl: "",
    loading: false,
    error: "",
  },

  onLoad(query) {
    const camera = decodeURIComponent(query.camera || "");
    this.setData({ camera });
    wx.setNavigationBarTitle({ title: camera || "实时预览" });
    this.refresh();
  },

  refresh() {
    const { camera } = this.data;
    if (!camera) {
      this.setData({ error: "缺少 camera 参数" });
      return;
    }
    this.setData({ loading: true, error: "", flvUrl: "" });
    api
      .playUrl(camera)
      .then((data) => {
        this.setData({ flvUrl: data.url || "", loading: false });
        if (!data.url) {
          this.setData({ error: "play 未返回 url" });
        }
      })
      .catch((e) => {
        this.setData({ error: String(e.message || e), loading: false });
      });
  },

  onState(e) {
    console.log("live-player state", e.detail);
  },

  onPlayerError(e) {
    console.error("live-player error", e.detail);
    this.setData({ error: `播放器错误: ${JSON.stringify(e.detail)}` });
  },
});
