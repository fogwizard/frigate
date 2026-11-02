const $ = (s) => document.querySelector(s);

function ensurePager(id, newerId, rangeId, olderId) {
  let bar = $("#" + id);
  if (!bar) {
    bar = document.createElement("div");
    bar.id = id;
    bar.className = "pager";
    bar.hidden = true;
    bar.innerHTML =
      `<button id="${newerId}" type="button" disabled>上翻50条</button>` +
      `<span id="${rangeId}"></span>` +
      `<button id="${olderId}" type="button">下翻50条</button>`;
    const health = $("#health");
    if (health) health.before(bar);
    else document.querySelector(".top")?.appendChild(bar);
  }
  return bar;
}

function ensureEventsPager() {
  return ensurePager("events-pager", "events-newer", "events-range", "events-older");
}

function ensureRecordingsPager() {
  return ensurePager("recordings-pager", "recordings-newer", "recordings-range", "recordings-older");
}

function switchPage(name) {
  document.querySelectorAll(".page").forEach((p) => p.classList.remove("active"));
  document.querySelectorAll("nav button").forEach((b) => b.classList.remove("active"));
  $(`#page-${name}`).classList.add("active");
  document.querySelector(`nav button[data-page="${name}"]`).classList.add("active");
  const eventsPager = ensureEventsPager();
  if (eventsPager) eventsPager.hidden = name !== "events";
  const recordingsPager = ensureRecordingsPager();
  if (recordingsPager) recordingsPager.hidden = name !== "recordings";
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
  if (!video || video.videoWidth <= 0 || video.readyState < 2 || video.paused || video.ended) {
    return false;
  }
  const now = performance.now();
  const prevT = Number(video.dataset.playT);
  const prevAt = Number(video.dataset.playAt || "0");
  if (!Number.isFinite(prevT) || video.currentTime > prevT + 0.02) {
    video.dataset.playT = String(video.currentTime);
    video.dataset.playAt = String(now);
    return video.currentTime > 0.05;
  }
  return prevAt > 0 && now - prevAt < 1000 && prevT > 0.05;
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
      const showSnap = () => {
        const img = card.querySelector("img");
        if (!img || img.classList.contains("dead") || img.naturalWidth <= 0) return;
        video.style.display = "none";
      };
      video.addEventListener("playing", () => {
        if (videoShowing(video)) showLive();
      });
      let lastSeek = 0;
      const stayLive = () => {
        if (videoShowing(video)) showLive();
        else showSnap();
        if (!video.buffered || !video.buffered.length) return;
        const end = video.buffered.end(video.buffered.length - 1);
        const lag = end - video.currentTime;
        const now = performance.now();
        // A short seek lands on the previous keyframe and the picture flashes
        // between that old frame and the live edge.
        if (lag > 3 && Number.isFinite(lag) && now - lastSeek > 2000) {
          lastSeek = now;
          video.currentTime = Math.max(0, end - 0.2);
        }
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
    const img = card.querySelector("img");
    if (!img) return;
    if (videoShowing(video)) {
      img.classList.add("dead");
      setSignal(card, true);
      return;
    }
    if (img.dataset.loading === "1") return;
    const gen = (Number(img.dataset.gen) || 0) + 1;
    img.dataset.gen = String(gen);
    img.dataset.loading = "1";
    const cam = card.dataset.cam;
    const next = new Image();
    next.onload = () => {
      if (Number(img.dataset.gen) === gen) img.dataset.loading = "0";
      if (Number(img.dataset.gen) !== gen || videoShowing(card.querySelector("video"))) {
        if (videoShowing(card.querySelector("video"))) img.classList.add("dead");
        return;
      }
      img.src = next.src;
      img.classList.remove("dead");
      setSignal(card, true);
    };
    next.onerror = () => {
      if (Number(img.dataset.gen) === gen) img.dataset.loading = "0";
      if (Number(img.dataset.gen) !== gen || videoShowing(card.querySelector("video"))) return;
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

const EVENTS_PAGE = 50;
let eventsOffset = 0;

function setEventsPager(total, shown) {
  const newer = $("#events-newer");
  const older = $("#events-older");
  const range = $("#events-range");
  if (newer) newer.disabled = eventsOffset <= 0;
  if (older) older.disabled = eventsOffset + shown >= total;
  if (range) {
    range.textContent = total
      ? `${eventsOffset + 1}–${eventsOffset + shown} / ${total}`
      : "0 条";
  }
}

async function loadEvents() {
  const list = $("#events-list");
  try {
    const r = await fetch(`/api/events?limit=${EVENTS_PAGE}&offset=${eventsOffset}`);
    const j = await r.json();
    const events = j.events || [];
    const total = Number.isFinite(Number(j.total)) ? Number(j.total) : eventsOffset + events.length;
    setEventsPager(total, events.length);
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

const RECORDINGS_PAGE = 50;
let recordingsOffset = 0;

function setRecordingsPager(total, shown) {
  const newer = $("#recordings-newer");
  const older = $("#recordings-older");
  const range = $("#recordings-range");
  if (newer) newer.disabled = recordingsOffset <= 0;
  if (older) older.disabled = recordingsOffset + shown >= total;
  if (range) {
    range.textContent = total
      ? `${recordingsOffset + 1}–${recordingsOffset + shown} / ${total}`
      : "0 条";
  }
}

async function loadRecordings() {
  const list = $("#rec-list");
  try {
    const r = await fetch(`/api/recordings?limit=${RECORDINGS_PAGE}&offset=${recordingsOffset}`);
    const j = await r.json();
    const rows = j.recordings || [];
    const total = Number.isFinite(Number(j.total)) ? Number(j.total) : recordingsOffset + rows.length;
    setRecordingsPager(total, rows.length);
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

ensureEventsPager();
ensureRecordingsPager();
document.addEventListener("click", (ev) => {
  const eventsNewer = ev.target.closest("#events-newer");
  const eventsOlder = ev.target.closest("#events-older");
  if (eventsNewer || eventsOlder) {
    if (eventsNewer?.disabled || eventsOlder?.disabled) return;
    eventsOffset = eventsNewer ? Math.max(0, eventsOffset - EVENTS_PAGE) : eventsOffset + EVENTS_PAGE;
    loadEvents();
    return;
  }
  const recordingsNewer = ev.target.closest("#recordings-newer");
  const recordingsOlder = ev.target.closest("#recordings-older");
  if (!recordingsNewer && !recordingsOlder) return;
  if (recordingsNewer?.disabled || recordingsOlder?.disabled) return;
  recordingsOffset = recordingsNewer
    ? Math.max(0, recordingsOffset - RECORDINGS_PAGE)
    : recordingsOffset + RECORDINGS_PAGE;
  loadRecordings();
});

loadLive();
refreshHealth();
setInterval(refreshHealth, 5000);
setInterval(refreshSnapshots, 200);
setInterval(refreshBoxes, 500);
setInterval(() => {
  if ($("#page-live").classList.contains("active") && !$("#live-grid .card")) loadLive();
}, 10000);
