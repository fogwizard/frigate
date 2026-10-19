const api = require("../../utils/api");

Page({
  data: {
    cameras: [],
    loading: false,
    error: "",
  },

  onShow() {
    this.reload();
  },

  reload() {
    this.setData({ loading: true, error: "" });
    api
      .listStreams()
      .then((data) => {
        this.setData({ cameras: data.cameras || [], loading: false });
      })
      .catch((e) => {
        this.setData({ error: String(e.message || e), loading: false, cameras: [] });
      });
  },

  openLive(e) {
    const cam = e.currentTarget.dataset.cam;
    wx.navigateTo({ url: `/pages/live/live?camera=${encodeURIComponent(cam)}` });
  },
});
