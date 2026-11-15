function parseBoardingCode(result) {
  let data;
  try {
    if (typeof result !== "string" || result.length > 2048) throw new Error();
    data = JSON.parse(result);
  } catch (error) {
    throw new Error("请扫描门店寄养二维码");
  }
  if (!data || data.type !== "petshop-boarding" || data.version !== 1 ||
      typeof data.token !== "string" || !/^[A-Za-z0-9_-]{16,512}$/.test(data.token)) {
    throw new Error("请扫描门店寄养二维码");
  }
  return data.token;
}

function normalizeCages(data) {
  if (!data || typeof data.storeName !== "string" || !Array.isArray(data.cages)) {
    throw new Error("笼位列表格式错误");
  }
  const seen = new Set();
  return data.cages.filter((cage) => {
    if (!cage || cage.available !== true || typeof cage.id !== "string" || !cage.id ||
        typeof cage.number !== "string" || !cage.number || !Number.isSafeInteger(cage.amount) ||
        cage.amount <= 0 || seen.has(cage.id)) return false;
    seen.add(cage.id);
    return true;
  }).slice(0, 64).map((cage) => ({
    id: cage.id, number: cage.number, amount: cage.amount,
    label: `${cage.number}号 · ¥${(cage.amount / 100).toFixed(2)}`,
  }));
}

function paymentParameters(data) {
  const payment = data && data.payment;
  if (!payment || !["timeStamp", "nonceStr", "package", "paySign"].every((key) =>
    typeof payment[key] === "string" && payment[key]) ||
    !["RSA", "MD5", "HMAC-SHA256"].includes(payment.signType) || !payment.package.startsWith("prepay_id=")) {
    throw new Error("支付参数无效，请查询订单状态");
  }
  return { timeStamp: payment.timeStamp, nonceStr: payment.nonceStr, package: payment.package,
    signType: payment.signType, paySign: payment.paySign };
}

module.exports = { parseBoardingCode, normalizeCages, paymentParameters };