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

async function loadLive() {
  const grid = $("#live-grid");
  try {
    const r = await fetch("/api/cameras");
    const j = await r.json();
    const cams = j.cameras || [];
    if (!cams.length) {
      grid.innerHTML = '<div class="empty">未配置摄像头</div>';
      return;
    }
    grid.innerHTML = cams
      .map(
        (c) => `
      <div class="card" data-cam="${c}">
        <header><span>${c}</span><span class="meta nosignal">无信号</span></header>
        <div class="frame">
          <img src="/api/snapshots/${encodeURIComponent(c)}?t=${Date.now()}" alt="${c}"
               onload="this.closest('.card').querySelector('.meta').textContent='live';this.closest('.card').querySelector('.meta').classList.remove('nosignal')"
               onerror="this.classList.add('dead');this.closest('.card').querySelector('.meta').textContent='无信号';this.closest('.card').querySelector('.meta').classList.add('nosignal')" />
          <div class="placeholder">等待 RTSP 快照…<br/>请在 config 填写可达的摄像头地址</div>
        </div>
      </div>`
      )
      .join("");
  } catch (e) {
    grid.innerHTML = `<div class="empty">加载失败: ${e}</div>`;
  }
}

function refreshSnapshots() {
  document.querySelectorAll("#live-grid img").forEach((img) => {
    const card = img.closest("[data-cam]");
    if (!card) return;
    const cam = card.dataset.cam;
    const next = new Image();
    next.onload = () => {
      img.src = next.src;
      img.classList.remove("dead");
      const meta = card.querySelector(".meta");
      if (meta) {
        meta.textContent = "live";
        meta.classList.remove("nosignal");
      }
    };
    next.onerror = () => {
      img.classList.add("dead");
      const meta = card.querySelector(".meta");
      if (meta) {
        meta.textContent = "无信号";
        meta.classList.add("nosignal");
      }
    };
    next.src = `/api/snapshots/${encodeURIComponent(cam)}?t=${Date.now()}`;
  });
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
        const snap = e.snapshot_path
          ? `<img src="/api/snapshots/${encodeURIComponent(e.camera)}" alt="" />`
          : `<img alt="" />`;
        const end = e.end_time ? new Date(e.end_time).toLocaleString() : "进行中";
        return `<div class="row">
          ${snap}
          <div>
            <div class="title">${e.label} · ${e.camera}</div>
            <div class="sub">${new Date(e.start_time).toLocaleString()} → ${end} · score ${(e.score * 100).toFixed(0)}%</div>
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
          <img alt="" />
          <div>
            <div class="title">${s.camera}</div>
            <div class="sub">${new Date(s.start_time).toLocaleString()} → ${end}<br/><code style="font-size:0.75rem;color:#8b9aab">${s.path}</code></div>
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
setInterval(() => {
  if ($("#page-live").classList.contains("active") && !$("#live-grid .card")) loadLive();
}, 10000);
