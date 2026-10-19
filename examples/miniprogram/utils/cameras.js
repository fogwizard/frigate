const MAX_CAMERAS = 64;

function normalizeCameras(data) {
  if (!data || !Array.isArray(data.cameras)) {
    throw new Error("镜头列表格式错误");
  }
  const cameras = [];
  const seen = new Set();
  for (const item of data.cameras) {
    const camera = typeof item === "string" ? item : item && item.camera;
    if (typeof camera !== "string" || !camera.trim() || seen.has(camera)) continue;
    const number = typeof item === "object" && item.number != null ? String(item.number) : String(cameras.length + 1).padStart(2, "0");
    const label = typeof item === "object" && typeof item.label === "string" ? item.label : camera;
    cameras.push({ camera, number, label: `${number}号 · ${label}`,
      ...(typeof item === "object" && Object.prototype.hasOwnProperty.call(item, "canControl")
        ? { canControl: item.canControl === true } : {}) });
    seen.add(camera);
    if (cameras.length === MAX_CAMERAS) break;
  }
  return cameras;
}

module.exports = { MAX_CAMERAS, normalizeCameras };