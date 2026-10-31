const $ = (s) => document.querySelector(s);

function switchPage(name) {
  document.querySelectorAll(".page").forEach((p) => p.classList.remove("active"));
  document.querySelectorAll("nav button").forEach((b) => b.classList.remove("active"));
  $(`#page-${name}`).classList.add("active");
  document.querySelector(`nav button[data-page="${name}"]`).classList.add("active");
  if (name === "events") loadEvents();
  if (name === "recordings") loadRecordings();
}

document.querySelectorAll("nav button").forEach((btn) => {
  btn.addEventListener("click", () => switchPage(btn.dataset.page));
});

async function refreshHealth() {
  const el = $("#health");
  try {
    const r = await fetch("/api/status");
    const j = await r.json();
    const infer = j.health.infer_connected ? "infer✓" : "infer✗";
    el.textContent = `v${j.health.version} · ${j.health.cameras} cams · ${infer} · up ${j.health.uptime_secs}s`;
    el.className = "health " + (j.health.ok ? "ok" : "bad");
  } catch {
    el.textContent = "offline";
    el.className = "health bad";
  }
}

function setSignal(card, ok) {
  const meta = card.querySelector(".meta");
  if (!meta) return;
  meta.textContent = ok ? "live" : "无信号";
  meta.classList.toggle("nosignal", !ok);
}

function videoShowing(video) {
  return !!(
    video &&
    video.style.display !== "none" &&
    video.videoWidth > 0 &&
    video.readyState >= 2 &&
    !video.paused &&
    !video.ended
  );
}

async function loadLive() {
  const grid = $("#live-grid");
  try {
    const r = await fetch("/api/cameras");
    const j = await r.json();
    const streams = j.streams || (j.cameras || []).map((c) => ({ name: c, live_url: "" }));
    if (!streams.length) {
      grid.innerHTML = '<div class="empty">未配置摄像头</div>';
      return;
    }
    grid.innerHTML = streams
      .map(
        (s) => `
      <div class="card" data-cam="${s.name}">
        <header><span>${s.name}</span><span class="meta nosignal">无信号</span></header>
        <div class="frame">
          <video src="${s.live_url}" autoplay muted playsinline></video>
          <img alt="${s.name}" class="dead" />
          <div class="boxes"></div>
          <div class="placeholder">等待画面…</div>
        </div>
      </div>`
      )
      .join("");
    grid.querySelectorAll("video").forEach((video) => {
      const card = video.closest(".card");
      const showLive = () => {
        video.style.display = "block";
        card.querySelector("img").classList.add("dead");
        setSignal(card, true);
      };
      video.addEventListener("playing", showLive);
      const stayLive = () => {
        if (videoShowing(video)) setSignal(card, true);
        if (!video.buffered || !video.buffered.length) return;
        const end = video.buffered.end(video.buffered.length - 1);
        const lag = end - video.currentTime;
        if (lag > 0.4 && Number.isFinite(lag)) video.currentTime = Math.max(0, end - 0.15);
      };
      video.addEventListener("progress", stayLive);
      setInterval(stayLive, 250);
      video.addEventListener("error", () => {
        if (videoShowing(video)) {
          setSignal(card, true);
          return;
        }
        video.style.display = "none";
        const img = card.querySelector("img");
        if (img && !img.classList.contains("dead") && img.naturalWidth > 0) return;
        setSignal(card, false);
      });
      if (videoShowing(video)) showLive();
    });
  } catch (e) {
    grid.innerHTML = `<div class="empty">加载失败: ${e}</div>`;
  }
}

function refreshSnapshots() {
  document.querySelectorAll("#live-grid .card").forEach((card) => {
    const video = card.querySelector("video");
    if (videoShowing(video)) {
      setSignal(card, true);
      return;
    }
    const img = card.querySelector("img");
    if (!img) return;
    const cam = card.dataset.cam;
    const next = new Image();
    next.onload = () => {
      img.src = next.src;
      img.classList.remove("dead");
      setSignal(card, true);
    };
    next.onerror = () => {
      if (videoShowing(video)) return;
      if (img.naturalWidth > 0 && !img.classList.contains("dead")) return;
      img.classList.add("dead");
      setSignal(card, false);
    };
    next.src = `/api/snapshots/${encodeURIComponent(cam)}?t=${Date.now()}`;
  });
}

function visibleMedia(card) {
  const video = card.querySelector("video");
  const img = card.querySelector("img");
  if (video && video.style.display !== "none" && video.videoWidth > 0) return video;
  if (img && !img.classList.contains("dead") && img.naturalWidth > 0) return img;
  return null;
}

function placeBoxes(card, host) {
  const media = visibleMedia(card);
  const frame = card.querySelector(".frame");
  if (!media || !frame) return;
  const fw = frame.clientWidth;
  const fh = frame.clientHeight;
  const nw = media.videoWidth || media.naturalWidth;
  const nh = media.videoHeight || media.naturalHeight;
  if (!fw || !fh || !nw || !nh) return;
  const fit = getComputedStyle(media).objectFit;
  const scale = fit === "cover" ? Math.max(fw / nw, fh / nh) : Math.min(fw / nw, fh / nh);
  const dw = nw * scale;
  const dh = nh * scale;
  host.style.left = `${(fw - dw) / 2}px`;
  host.style.top = `${(fh - dh) / 2}px`;
  host.style.right = "auto";
  host.style.bottom = "auto";
  host.style.width = `${dw}px`;
  host.style.height = `${dh}px`;
}

async function refreshBoxes() {
  try {
    const r = await fetch("/api/status");
    const j = await r.json();
    const byName = new Map((j.cameras || []).map((c) => [c.name, c.objects || []]));
    document.querySelectorAll("#live-grid .card").forEach((card) => {
      const host = card.querySelector(".boxes");
      if (!host) return;
      const objects = byName.get(card.dataset.cam) || [];
      placeBoxes(card, host);
      host.innerHTML = objects
        .map((o) => {
          const [x1, y1, x2, y2] = o.xyxy;
          const style = `left:${x1 * 100}%;top:${y1 * 100}%;width:${(x2 - x1) * 100}%;height:${(y2 - y1) * 100}%`;
          return `<div class="box" style="${style}"><span>${o.label} ${(o.score * 100).toFixed(0)}%</span></div>`;
        })
        .join("");
    });
  } catch {
    /* status is optional while the page is open */
  }
}

async function loadEvents() {
  const list = $("#events-list");
  try {
    const r = await fetch("/api/events?limit=50");
    const j = await r.json();
    const events = j.events || [];
    if (!events.length) {
      list.innerHTML = '<div class="empty">暂无事件</div>';
      return;
    }
    list.innerHTML = events
      .map((e) => {
        const snap = `<img src="/api/events/${encodeURIComponent(e.id)}/snapshot" alt="" />`;
        const end = e.end_time ? new Date(e.end_time).toLocaleString() : "进行中";
        const clip = e.clip_path
          ? `<video src="/api/events/${encodeURIComponent(e.id)}/clip" controls preload="none"></video>`
          : "";
        return `<div class="row">
          ${snap}
          <div>
            <div class="title">${e.label} · ${e.camera}</div>
            <div class="sub">${new Date(e.start_time).toLocaleString()} → ${end} · score ${(e.score * 100).toFixed(0)}%</div>
            ${e.note ? `<div class="sub">${e.note}</div>` : ""}
            ${clip}
          </div>
          <span class="badge">${e.id.slice(0, 8)}</span>
        </div>`;
      })
      .join("");
  } catch (e) {
    list.innerHTML = `<div class="empty">加载失败: ${e}</div>`;
  }
}

async function loadRecordings() {
  const list = $("#rec-list");
  try {
    const r = await fetch("/api/recordings?limit=80");
    const j = await r.json();
    const rows = j.recordings || [];
    if (!rows.length) {
      list.innerHTML = '<div class="empty">暂无录像分段（需 FFmpeg 与可达 RTSP）</div>';
      return;
    }
    list.innerHTML = rows
      .map((s) => {
        const end = s.end_time ? new Date(s.end_time).toLocaleString() : "…";
        return `<div class="row">
          <video src="/api/recordings/${s.id}/file" controls preload="metadata"></video>
          <div>
            <div class="title">${s.camera}</div>
            <div class="sub">${new Date(s.start_time).toLocaleString()} → ${end}</div>
          </div>
          <span class="badge">#${s.id}</span>
        </div>`;
      })
      .join("");
  } catch (e) {
    list.innerHTML = `<div class="empty">加载失败: ${e}</div>`;
  }
}

loadLive();
refreshHealth();
setInterval(refreshHealth, 5000);
setInterval(refreshSnapshots, 200);
setInterval(refreshBoxes, 500);
setInterval(() => {
  if ($("#page-live").classList.contains("active") && !$("#live-grid .card")) loadLive();
}, 10000);
