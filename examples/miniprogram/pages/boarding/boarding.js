const api = require("../../utils/api");
const boarding = require("../../utils/boarding");
const { confirm } = require("../../utils/door");

Page({
  data: { busy: false, storeName: "", cages: [], cageIndex: 0, status: "", pending: false, canPay: false },

  async onLoad() {
    this.setData({ busy: true });
    try {
      await getApp().login();
      if (this.unloaded) return;
      this.storageKey = `boarding-order:${getApp().globalData.openid}`;
      this.pendingOrder = wx.getStorageSync(this.storageKey) || null;
      this.setData({ pending: !!this.pendingOrder, status: this.pendingOrder ? "有待确认的订单，请先查询支付结果" : "" });
    } catch (error) {
      if (!this.unloaded) this.setData({ status: error.message });
    } finally {
      if (!this.unloaded) this.setData({ busy: false });
    }
  },

  onUnload() { this.unloaded = true; },

  savePending(value) {
    wx.setStorageSync(this.storageKey, value);
    this.pendingOrder = value;
    if (!this.unloaded) this.setData({ pending: true });
  },

  clearPending() {
    wx.removeStorageSync(this.storageKey);
    this.pendingOrder = null;
    this.setData({ pending: false, canPay: false });
  },

  async scanBoarding() {
    if (this.data.busy || this.data.pending || this.unloaded) return;
    this.setData({ busy: true, status: "", cages: [], storeName: "" });
    try {
      await getApp().login();
      if (!this.storageKey) this.storageKey = `boarding-order:${getApp().globalData.openid}`;
      if (this.unloaded) return;
      const result = await new Promise((resolve, reject) => wx.scanCode({
        onlyFromCamera: true, scanType: ["qrCode"], success: resolve,
        fail(error) {
          if (/cancel/i.test(error.errMsg || "")) resolve(null);
          else reject(new Error("无法扫码，请检查相机权限"));
        },
      }));
      if (!result || this.unloaded) return;
      const qrToken = boarding.parseBoardingCode(result.result);
      const data = await api.boardingCages(qrToken);
      if (this.unloaded) return;
      const cages = boarding.normalizeCages(data);
      this.qrToken = qrToken;
      this.setData({ cages, cageIndex: 0, storeName: data.storeName, status: cages.length ? "" : "暂无可用笼位" });
    } catch (error) {
      if (!this.unloaded) this.setData({ status: error.message });
    } finally {
      if (!this.unloaded) this.setData({ busy: false });
    }
  },

  selectCage(event) {
    const cageIndex = Number(event.detail.value);
    if (!this.data.busy && !this.data.pending && Number.isInteger(cageIndex) && this.data.cages[cageIndex]) {
      this.setData({ cageIndex });
    }
  },

  async payBoarding() {
    if (this.unloaded || this.data.busy) return;
    const cage = this.data.cages[this.data.cageIndex];
    if (!this.pendingOrder && (!cage || !this.qrToken || !this.storageKey)) return;
    if (this.pendingOrder && !this.data.canPay) return;
    this.setData({ busy: true, status: "", canPay: false });
    let paymentStarted = false;
    try {
      const allowed = await confirm({ title: "确认寄养支付", content: this.pendingOrder
        ? "继续支付当前寄养订单？" : `${this.data.storeName} · ${cage.label}，确认支付该次寄养费用？`, confirmText: "确认支付" });
      if (!allowed || this.unloaded) return;
      await getApp().login();
      if (this.unloaded) return;
      if (!this.pendingOrder) {
        this.savePending({ qrToken: this.qrToken, cageId: cage.id,
          requestId: `boarding-${Date.now()}-${Math.random().toString(36).slice(2)}` });
      }
      let order;
      if (!this.pendingOrder.orderId) {
        order = await api.createBoardingOrder(this.pendingOrder.qrToken, this.pendingOrder.cageId, this.pendingOrder.requestId);
        if (!order || typeof order.orderId !== "string" || !order.orderId) throw new Error("订单结果未确认，请查询支付结果");
        this.savePending({ ...this.pendingOrder, orderId: order.orderId });
      } else {
        order = await api.boardingOrder(this.pendingOrder.orderId);
      }
      if (this.unloaded) return;
      if (order.paymentStatus !== "pending" || order.status !== "pending") {
        await this.resolveOrder();
        return;
      }
      if (!Number.isSafeInteger(order.amount) || order.amount <= 0) {
        throw new Error("订单金额无效，请联系门店");
      }
      const payment = boarding.paymentParameters(order);
      const finalConfirmed = await confirm({ title: "确认订单金额",
        content: `订单 ${this.pendingOrder.orderId}，本次支付 ¥${(order.amount / 100).toFixed(2)}`,
        confirmText: "去支付" });
      if (!finalConfirmed || this.unloaded) return;
      paymentStarted = true;
      await new Promise((resolve, reject) => wx.requestPayment({ ...payment, success: resolve, fail: reject }));
      if (!this.unloaded) await this.resolveOrder();
    } catch (error) {
      if (!this.unloaded) this.setData({ status: paymentStarted
        ? "支付结果待确认，请查询支付结果，勿重复付款"
        : `${error.message || "创建订单失败"}。如已有订单，请先查询结果` });
    } finally {
      if (!this.unloaded) this.setData({ busy: false });
    }
  },

  async checkPayment() {
    if (this.data.busy || this.unloaded || !this.pendingOrder) return;
    this.setData({ busy: true, canPay: false });
    try {
      await getApp().login();
      if (this.unloaded) return;
      if (!this.pendingOrder.orderId) {
        const order = await api.createBoardingOrder(this.pendingOrder.qrToken, this.pendingOrder.cageId, this.pendingOrder.requestId);
        if (!order || typeof order.orderId !== "string" || !order.orderId) throw new Error("订单结果未确认");
        this.savePending({ ...this.pendingOrder, orderId: order.orderId });
      }
      if (!this.unloaded) await this.resolveOrder();
    } catch (error) {
      if (!this.unloaded) this.setData({ status: `${error.message || "查询失败"}，请稍后查询或联系门店` });
    } finally {
      if (!this.unloaded) this.setData({ busy: false });
    }
  },

  async resolveOrder() {
    const order = await api.boardingOrder(this.pendingOrder.orderId);
    if (this.unloaded) return;
    if (order && order.paymentStatus === "paid" && order.status === "active" &&
        typeof order.recordId === "string" && order.recordId && typeof order.camera === "string" && order.camera) {
      const data = await api.boardingCameras();
      if (this.unloaded) return;
      if (!data.cameras.some((item) => item.camera === order.camera)) {
        this.setData({ status: "已支付，寄养权限尚未同步，请稍后查询" });
        return;
      }
      this.clearPending();
      this.setData({ status: "寄养已生效" });
      wx.redirectTo({ url: `/pages/live/live?camera=${encodeURIComponent(order.camera)}` });
    } else if (order && ["closed", "cancelled", "refunded"].includes(order.status)) {
      this.clearPending();
      this.setData({ status: "订单已关闭或退款，可重新扫码寄养" });
    } else {
      this.setData({ status: order && order.paymentStatus === "paid"
        ? "已支付，等待寄养生效，请稍后查询" : "支付尚未确认，请查询结果或继续支付当前订单",
      canPay: !!order && order.paymentStatus === "pending" && order.status === "pending" });
    }
  },
});