(() => {
  const { core, event, dialog, window: tauriWindow } = window.__TAURI__;
  const invoke = core.invoke;
  const $ = (id) => document.getElementById(id);

  // =========================================================================
  // 1. STATE & CONSTANTS
  // =========================================================================
  const APP_ICON_SVG = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 512 512" width="100%" height="100%">
    <defs>
      <linearGradient id="icBg" x1="0" y1="0" x2="1" y2="1"><stop offset="0%" stop-color="#272726"/><stop offset="55%" stop-color="#191918"/><stop offset="100%" stop-color="#0F0F0E"/></linearGradient>
      <radialGradient id="icGlow" cx="62%" cy="48%" r="52%"><stop offset="0%" stop-color="#F59E0B" stop-opacity="0.22"/><stop offset="55%" stop-color="#D97706" stop-opacity="0.07"/><stop offset="100%" stop-color="#141414" stop-opacity="0"/></radialGradient>
      <linearGradient id="icBack" x1="0" y1="0" x2="1" y2="1"><stop offset="0%" stop-color="#A3A3A3" stop-opacity="0.34"/><stop offset="100%" stop-color="#525252" stop-opacity="0.14"/></linearGradient>
      <linearGradient id="icMid" x1="0" y1="0" x2="1" y2="1"><stop offset="0%" stop-color="#D4D4D4" stop-opacity="0.46"/><stop offset="100%" stop-color="#737373" stop-opacity="0.20"/></linearGradient>
      <linearGradient id="icPlay" x1="0" y1="0" x2="1" y2="1"><stop offset="0%" stop-color="#FAFAFA"/><stop offset="55%" stop-color="#E5E5E5"/><stop offset="100%" stop-color="#C8C4BC"/></linearGradient>
      <linearGradient id="icMoon" x1="0" y1="0" x2="1" y2="1"><stop offset="0%" stop-color="#FDE68A"/><stop offset="100%" stop-color="#F59E0B"/></linearGradient>
    </defs>
    <rect width="512" height="512" fill="url(#icBg)"/>
    <rect width="512" height="512" fill="url(#icGlow)"/>
    <path d="M0 0 H512 V180 Q256 240 0 155 Z" fill="#FFFFFF" opacity="0.035"/>
    <rect x="86" y="138" width="166" height="236" rx="34" fill="url(#icBack)" stroke="#A3A3A3" stroke-opacity="0.28" stroke-width="5"/>
    <rect x="142" y="156" width="166" height="200" rx="32" fill="url(#icMid)" stroke="#E5E5E5" stroke-opacity="0.38" stroke-width="5"/>
    <path d="M236 154 C236 135 256 124 272 134 L414 222 C430 232 430 256 414 266 L272 354 C256 364 236 353 236 334 Z" transform="translate(0, 12)" fill="url(#icPlay)"/>
    <path d="M244 166 C244 152 258 144 270 151 L396 229 C408 236 408 252 396 259 L270 337 C258 344 244 336 244 322 Z" transform="translate(0, 12)" fill="none" stroke="#FFFFFF" stroke-opacity="0.45" stroke-width="4"/>
    <path d="M394 102 A36 36 0 1 1 352 144 A28 28 0 1 0 394 102 Z" fill="url(#icMoon)" opacity="0.95"/>
  </svg>`;

  const MODE_META = {
    explore: {
      label: "Explore",
      fullLabel: "Explore & Triage",
      desc: "Sort largest first, preview scenes, protect important videos, and tag the rest.",
      icon: `<svg width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="m21 21-5.197-5.197m0 0A7.5 7.5 0 1 0 5.196 5.196a7.5 7.5 0 0 0 10.607 10.607Z"/></svg>`,
    },
    encode: {
      label: "Transcode",
      fullLabel: "Videos to Transcode",
      desc: "Filter & configure videos, then add selected files to the Encode Queue.",
      icon: `<svg width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M3.75 13.5l10.5-11.25L12 10.5h8.25L9.75 21.75 12 13.5H3.75z"/></svg>`,
    },
    organize: {
      label: "Organize",
      fullLabel: "Organize Collection",
      desc: "Largest files, exact & similar duplicates (SQLite cache), watermark & face tags, sort into folders.",
      icon: `<svg width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M3.75 9.776c.112-.017.227-.026.344-.026h15.812c.117 0 .232.009.344.026m-16.5 0a2.25 2.25 0 0 0-1.883 2.542l.857 6a2.25 2.25 0 0 0 2.227 1.932H19.05a2.25 2.25 0 0 0 2.227-1.932l.857-6a2.25 2.25 0 0 0-1.883-2.542m-16.5 0V6A2.25 2.25 0 0 1 6 3.75h3.879a1.5 1.5 0 0 1 1.06.44l2.122 2.12a1.5 1.5 0 0 0 1.06.44H18A2.25 2.25 0 0 1 20.25 9v.776"/></svg>`,
    },
    repair: {
      label: "Repair",
      fullLabel: "Repair Videos",
      desc: "Diagnose atoms & recover unseekable or truncated MP4/MOV clips non-destructively.",
      icon: `<svg width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M11.42 15.17 17.25 21A2.652 2.652 0 0 0 21 17.25l-5.877-5.877M11.42 15.17l2.496-3.03c.317-.384.74-.626 1.208-.766M11.42 15.17l-4.655 5.653a2.548 2.548 0 1 1-3.586-3.586l6.837-5.63"/></svg>`,
    },
  };

  let snapshot = null;
  // Rebuilt once per snapshot. Queue rows consult these O(1) indexes instead
  // of scanning every job in every other tab during each table render.
  let protectedSourceSet = new Set();
  let sharedSourceQueue = new Map();
  let encodeRowNodes = new Map();
  let encodeJobsById = new Map();
  let renderedEncodeJobs = [];
  let renderedEncodeAllJobs = [];
  let selectedJobId = null;
  let sidebarOpen = true;
  let sidebarMode = "setup"; // "setup" | "job"
  let exploreFilterMode = "all"; // "all" | "unreviewed" | "compress" | "protected" | "repair"
  let sortMode = "manual"; // "manual" | "largest" | "bitrate" (Explore)
  let encodeSort = { col: null, dir: "desc" }; // Transcode table column sort
  let liveSpeedJobId = null;
  let liveSpeedHistory = [];
  const encodeFilters = {
    search: "",
    searchMode: "name", // "name" | "path" | "regex"
    searchInvert: false,
    codec: "all",
    selectedCodecs: new Set(),
    resOp: "ge",
    resHeight: 0,
    durOp: "ge",
    durSecs: 0,
    durUnit: "m",
    sizeOp: "ge",
    minMb: 0,
    sizeUnit: "GB",
    bitrateOp: "ge",
    minMbps: 0,
    target: "all",
    status: "all",
  };
  let activeColPopover = null; // DOM ref for column filter popover
  let dedupeAlsoSha256 = true;
  let dedupePerceptual = true;
  let dedupeScanning = false;
  let organizeSideTab = "largest"; // "largest" | "duplicates" | "watermarks" | "faces" | "folder" | "library" | "catalog_dups" | "batch" | "catalog_people" | "integrity" | "tasks"
  let organizeOverview = null;
  let organizeOverviewQueueId = null;
  let latestTasksCount = 0;
  let catalogFacesEnabled = false;
  const EMBEDDED_ORG_PANELS = {
    library: "view-library",
    catalog_dups: "view-duplicates",
    batch: "view-organize",
    catalog_people: "view-people",
    integrity: "view-repair",
    tasks: "view-tasks",
  };
  let ocrScanning = false;
  let faceScanning = false;
  const orgSelectedByQueue = new Map(); // queue_id -> Set<job_id>
  function getOrgSelectedSet(qid = activeQueueId) {
    if (!qid) return new Set();
    let s = orgSelectedByQueue.get(qid);
    if (!s) {
      s = new Set();
      orgSelectedByQueue.set(qid, s);
    }
    return s;
  }
  let moveFolderTargetDir = "";
  let moveSelectedTag = "";
  let moveCopyOnly = false;
  let movePreserveSubfolders = false;
  let moveCollisionPolicy = "keep_existing";
  let movePreviewResult = null;
  let settingsTabOpen = false;
  let currentPlatformView = "transcode";

  const expandedJobIds = new Set();
  const checkedJobIds = new Set();
  const queueCheckedIds = new Set();
  const encodeSubViewByTab = new Map(); // queue_id -> "list" | "queue"
  let queuePageFilter = "all"; // "all" | "active" | "completed" | "failed"
  let queuePageNodes = new Map();

  function activeEncodeSubView(q) {
    return (q && encodeSubViewByTab.get(q.id)) || "list";
  }

  function setActiveEncodeSubView(q, subView) {
    if (!q) return;
    encodeSubViewByTab.set(q.id, subView === "queue" ? "queue" : "list");
  }
  let queueJobsById = new Map();
  const storyboardCache = new Map(); // job_id -> array of data/blob URLs
  const storyboardInflight = new Set();
  const repairLiveProgress = new Map(); // job_id -> progress obj

  let stagingState = null;
  let livePreviewTimer = null;
  let currentLiveJobId = null;
  let openSampleRefresh = null;

  // =========================================================================
  // 2. SNAPSHOT & HELPERS
  // =========================================================================
  function escapeHtml(s) {
    if (s === null || s === undefined) return "";
    return String(s)
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;");
  }

  function fmtBytes(b) {
    if (b === null || b === undefined) return "—";
    if (b === 0) return "0 B";
    const u = ["B", "KB", "MB", "GB", "TB"];
    let v = b;
    let i = 0;
    while (v >= 1000 && i < u.length - 1) {
      v /= 1000;
      i++;
    }
    return `${v >= 100 ? v.toFixed(0) : v >= 10 ? v.toFixed(1) : v.toFixed(2)} ${u[i]}`;
  }

  function fmtDuration(s) {
    if (!s || !isFinite(s)) return "0:00";
    const h = Math.floor(s / 3600);
    const m = Math.floor((s % 3600) / 60);
    const sec = Math.floor(s % 60);
    const pad = (n) => String(n).padStart(2, "0");
    return h > 0 ? `${h}:${pad(m)}:${pad(sec)}` : `${m}:${pad(sec)}`;
  }

  function fmtRatio(inp, out) {
    if (!out || out <= 0) return "—";
    const pct = Math.round((1 - out / inp) * 100);
    return `${(inp / out).toFixed(2)}:1 · ${pct}% smaller`;
  }

  function jobFileName(j) {
    const p = j.source_rel_path || j.source_path || "";
    const parts = p.split(/[\\/]/);
    return parts[parts.length - 1] || p;
  }

  function jobVideoInfo(j) {
    const isPendingStats = !j.probe && j.status_reason === "Computing media stats…";
    const v = j.probe?.streams?.find((s) => s.codec_type === "video" && !s.disposition?.attached_pic);
    const dur = j.probe?.format?.duration_secs || 0;
    const br = j.probe?.format?.bit_rate || v?.bit_rate || (dur > 0 ? Math.round((j.input_bytes * 8) / dur) : 0);
    const mbps = br > 0 ? +(br / 1_000_000).toFixed(1) : 0;
    const codec = v?.codec_name ? v.codec_name.toUpperCase() : (isPendingStats ? "STATS…" : "VIDEO");
    const res = v?.width && v?.height ? `${v.width}×${v.height}` : (isPendingStats ? "Computing stats…" : "—");
    return { v, dur, br, mbps, codec, res, isPendingStats };
  }

  function activeQueue() {
    if (!snapshot) return null;
    return snapshot.queues.find((q) => q.id === snapshot.active_queue_id) || snapshot.queues[0] || null;
  }

  function isImagePath(p) {
    return /\.(jpg|jpeg|png|webp|bmp|gif|tiff?|heic|heif)$/i.test(p || "");
  }

  function activeMode(q) {
    const raw = (q?.tab_type || "encode").toLowerCase();
    return raw === "dedupe" ? "organize" : raw;
  }

  function activeTabMeta(q) {
    if (!q || !snapshot?.tab_meta) return { resource_source: null, forwarded_from: null, repair_refs: [], repair_video_only: false, repair_deep_scan: true, repair_timeout_secs: 300 };
    return snapshot.tab_meta[q.id] || { resource_source: null, forwarded_from: null, repair_refs: [], repair_video_only: false, repair_deep_scan: true, repair_timeout_secs: 300 };
  }

  function activeQueueJobs() {
    const q = activeQueue();
    if (!q || !snapshot) return [];
    return snapshot.jobs.filter((j) => j.queue_id === q.id);
  }

  function jobDecision(j) {
    return snapshot?.job_decisions?.[j.id] || "unreviewed";
  }

  function isProtectedSource(sourcePath) {
    return protectedSourceSet.has(sourcePath);
  }

  function sharedQueueName(j) {
    if (!snapshot) return null;
    if (isProtectedSource(j.source_path)) return "Protected ★";
    const owners = sharedSourceQueue.get(j.source_path);
    if (!owners) return null;
    if (owners.has(j.queue_id)) {
      for (const [queueId, name] of owners) if (queueId !== j.queue_id) return name;
      return null;
    }
    return owners.values().next().value || null;
  }

  function rebuildSourceLookups() {
    protectedSourceSet = new Set(snapshot?.protected_sources || []);
    sharedSourceQueue = new Map();
    if (!snapshot) return;
    const queueById = new Map(snapshot.queues.map((q) => [q.id, q]));
    for (const job of snapshot.jobs) {
      if (job.status === "completed" || !job.source_path) continue;
      let owners = sharedSourceQueue.get(job.source_path);
      if (!owners) sharedSourceQueue.set(job.source_path, owners = new Map());
      if (!owners.has(job.queue_id)) owners.set(job.queue_id, queueById.get(job.queue_id)?.name || "Other tab");
    }
    const queueOrder = new Map(snapshot.queues.map((q, index) => [q.id, index]));
    for (const [path, owners] of sharedSourceQueue) {
      if (owners.size > 1) {
        sharedSourceQueue.set(path, new Map([...owners].sort((a, b) => (queueOrder.get(a[0]) ?? 0) - (queueOrder.get(b[0]) ?? 0))));
      }
    }
  }

  function showToast(message, tone = "info", action = null) {
    const container = $("toast-container");
    if (!container) return;
    const el = document.createElement("div");
    el.className = `toast-item ${tone}`;
    const span = document.createElement("span");
    span.textContent = message;
    el.appendChild(span);
    if (action && action.label && typeof action.onClick === "function") {
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = "btn btn-sm";
      btn.style.background = "rgba(255,255,255,0.18)";
      btn.style.color = "#fff";
      btn.textContent = action.label;
      btn.onclick = () => {
        action.onClick();
        el.remove();
      };
      el.appendChild(btn);
    }
    container.appendChild(el);
    setTimeout(() => el.remove(), action ? 4600 : 3000);
  }

  function findQueueVolumeInfo(q) {
    if (!snapshot || !q) return null;
    if (q.destination_root) {
      const match = snapshot.volumes.find((v) => q.destination_root.startsWith(v.mount_point));
      if (match) return match;
    }
    const jobs = activeQueueJobs();
    if (jobs.length > 0 && jobs[0].dest_volume_id) {
      return snapshot.volumes.find((v) => v.volume_id === jobs[0].dest_volume_id) || snapshot.volumes[0];
    }
    return snapshot.volumes[0] || null;
  }

  function recordLiveSpeedPoint(ev) {
    if (!ev?.job_id) return;
    if (liveSpeedJobId !== ev.job_id) {
      liveSpeedJobId = ev.job_id;
      liveSpeedHistory = [];
    }
    if (ev.speed != null && ev.speed > 0) {
      liveSpeedHistory.push({
        elapsed_secs: ev.elapsed_secs || 0,
        out_time_secs: ev.out_time_secs || 0,
        speed: ev.speed,
        fps: ev.fps ?? null,
        bytes_written: ev.bytes_written || 0,
        est_final_ratio: ev.est_final_ratio ?? null,
      });
      if (liveSpeedHistory.length > 240) {
        liveSpeedHistory = liveSpeedHistory.filter((_, i, a) => i % 2 === 0 || i === a.length - 1);
      }
    }
  }

  async function refreshSnapshot() {
    snapshot = await invoke("get_snapshot");
    const ap = snapshot.active_progress;
    const srvHist = snapshot.active_speed_history || [];
    if (ap?.job_id) {
      if (liveSpeedJobId !== ap.job_id || srvHist.length >= liveSpeedHistory.length) {
        liveSpeedJobId = ap.job_id;
        liveSpeedHistory = srvHist.slice();
      }
    } else {
      liveSpeedJobId = null;
      liveSpeedHistory = [];
    }
    rebuildSourceLookups();
    const jobs = activeQueueJobs();
    if (!selectedJobId || !jobs.some((j) => j.id === selectedJobId)) {
      selectedJobId = jobs[0]?.id || null;
    }
    renderAll();
    const q = activeQueue();
    if (q && activeMode(q) === "organize") {
      loadOrganizeOverview(q.id, true);
    }
  }

  let orgOverviewInflight = null;
  async function loadOrganizeOverview(queueId, force = false) {
    if (!queueId) return;
    if (!force && organizeOverviewQueueId === queueId && organizeOverview) return;
    if (orgOverviewInflight === queueId && !force) return;
    orgOverviewInflight = queueId;
    try {
      const ov = await invoke("tab_get_organize_overview", { queueId });
      organizeOverview = ov;
      organizeOverviewQueueId = queueId;
      const tags = ov?.available_tags || [];
      if (tags.length > 0 && (!moveSelectedTag || !tags.includes(moveSelectedTag))) {
        moveSelectedTag = tags[0];
      }
      const q = activeQueue();
      if (q && q.id === queueId && activeMode(q) === "organize") {
        renderOrganizeView(q, activeQueueJobs());
        renderActionBar();
        renderSidebar();
      }
    } catch (_) {
    } finally {
      if (orgOverviewInflight === queueId) orgOverviewInflight = null;
    }
  }

  // =========================================================================
  // 3. TOP-LEVEL RENDERERS
  // =========================================================================
  function renderAll() {
    if (!snapshot) return;
    renderTabs();
    renderQueueBadges();
    renderActionBar();
    renderLineageBar();
    renderWorkspace();
    renderSidebar();
    renderFooter();
  }

  function renderTabs() {
    const strip = $("tabs-strip");
    strip.innerHTML = "";
    const queues = snapshot.queues || [];
    const activeIdx = queues.findIndex((q) => q.id === snapshot.active_queue_id);

    queues.forEach((q, idx) => {
      const isResourceActive = q.id === snapshot.active_queue_id && currentPlatformView === "transcode";
      const beforeActive = currentPlatformView === "transcode"
        ? idx === activeIdx - 1
        : (settingsTabOpen && currentPlatformView === "settings" && idx === queues.length - 1);
      const mode = activeMode(q);
      const qJobs = (snapshot.jobs || []).filter((j) => j.queue_id === q.id);
      const dedupeGroups = snapshot.tab_dedupe_groups?.[q.id] || [];
      const count = mode === "organize" && organizeSideTab === "duplicates" && dedupeGroups.length > 0
        ? dedupeGroups.reduce((a, g) => a + g.items.length, 0)
        : qJobs.length;

      const repairBusy = mode === "repair" && qJobs.some((j) => {
        const rs = snapshot.tab_repair_items?.[j.id]?.status;
        return rs === "diagnosing" || rs === "recovering";
      });
      const encBusy = mode === "encode" && q.is_running && !q.is_paused && qJobs.some((j) => ["encoding", "verifying", "publishing"].includes(j.status));
      const meta = MODE_META[mode] || MODE_META.encode;

      const item = document.createElement("div");
      item.className = "tab-item chrome-tab " + (isResourceActive ? "active chrome-tab-active" : "chrome-tab-inactive") + (beforeActive ? " hide-divider" : "");

      const btn = document.createElement("button");
      btn.type = "button";
      btn.role = "tab";
      btn.className = "tab-select-btn";
      btn.setAttribute("aria-selected", String(isResourceActive));

      const iconWrap = document.createElement("span");
      iconWrap.className = "tab-mode-icon";
      if (mode === "encode" && (encBusy || qJobs.some((j) => j.status === "encoding"))) {
        iconWrap.style.color = "#F59E0B";
      }
      iconWrap.title = meta.fullLabel;
      iconWrap.innerHTML = encBusy || repairBusy ? `<span class="dot pulse" style="margin-right:2px;"></span>${meta.icon}` : meta.icon;
      btn.appendChild(iconWrap);

      const label = document.createElement("span");
      label.className = "tab-label";
      label.textContent = q.name;
      btn.appendChild(label);

      const pill = document.createElement("span");
      pill.className = "tab-count-pill";
      pill.textContent = String(count);
      btn.appendChild(pill);

      btn.addEventListener("click", async () => {
        if (currentPlatformView !== "transcode") {
          window.dispatchEvent(new CustomEvent("ms-request-switch-view", { detail: { view: "transcode" } }));
        }
        if (q.id !== snapshot.active_queue_id) {
          await invoke("select_queue_tab", { queueId: q.id });
        } else {
          renderTabs();
        }
      });
      btn.addEventListener("dblclick", () => {
        openPromptModal("Rename Resource Tab", "Tab name:", q.name, async (next) => {
          await invoke("rename_queue_tab", { queueId: q.id, name: next });
        });
      });
      item.appendChild(btn);

      if (queues.length > 1) {
        const close = document.createElement("button");
        close.type = "button";
        close.className = "tab-close";
        close.innerHTML = `<svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2"><path stroke-linecap="round" stroke-linejoin="round" d="M6 18 18 6M6 6l12 12"/></svg>`;
        close.title = "Close tab";
        close.addEventListener("click", (e) => {
          e.stopPropagation();
          requestCloseTab(q, count, encBusy || repairBusy);
        });
        item.appendChild(close);
      }

      strip.appendChild(item);
    });

    // Special System Tab: Settings (pinned cleanly at the end of the strip when open)
    if (settingsTabOpen) {
      const isSettingsActive = currentPlatformView === "settings";
      const sItem = document.createElement("div");
      sItem.className = "tab-item chrome-tab " + (isSettingsActive ? "active chrome-tab-active" : "chrome-tab-inactive");

      const sBtn = document.createElement("button");
      sBtn.type = "button";
      sBtn.role = "tab";
      sBtn.className = "tab-select-btn";
      sBtn.setAttribute("aria-selected", String(isSettingsActive));
      sBtn.innerHTML = `<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path stroke-linecap="round" stroke-linejoin="round" d="M9.594 3.94c.09-.542.56-.94 1.11-.94h2.593c.55 0 1.02.398 1.11.94l.213 1.281c.063.374.313.686.645.87.074.04.147.083.22.127.325.196.72.257 1.075.124l1.217-.456a1.125 1.125 0 0 1 1.37.49l1.296 2.247a1.125 1.125 0 0 1-.26 1.431l-1.003.827c-.293.241-.438.613-.43.992a7.723 7.723 0 0 1 0 .255c-.008.378.137.75.43.991l1.004.827c.424.35.534.955.26 1.43l-1.298 2.247a1.125 1.125 0 0 1-1.369.491l-1.217-.456c-.355-.133-.75-.072-1.076.124a6.47 6.47 0 0 1-.22.128c-.331.183-.581.495-.644.869l-.213 1.281c-.09.543-.56.94-1.11.94h-2.594c-.55 0-1.019-.398-1.11-.94l-.213-1.281c-.062-.374-.312-.686-.644-.87a6.52 6.52 0 0 1-.22-.127c-.325-.196-.72-.257-1.076-.124l-1.217.456a1.125 1.125 0 0 1-1.369-.49l-1.297-2.247a1.125 1.125 0 0 1 .26-1.431l1.004-.827c.292-.24.437-.613.43-.991a6.932 6.932 0 0 1 0-.255c.007-.38-.138-.751-.43-.992l-1.004-.827a1.125 1.125 0 0 1-.26-1.43l1.297-2.247a1.125 1.125 0 0 1 1.37-.491l1.216.456c.356.133.751.072 1.076-.124.072-.044.146-.086.22-.128.332-.183.582-.495.644-.869l.214-1.28Z"/><path stroke-linecap="round" stroke-linejoin="round" d="M15 12a3 3 0 1 1-6 0 3 3 0 0 1 6 0Z"/></svg><span class="tab-label">Settings</span>`;
      sBtn.onclick = () => {
        window.dispatchEvent(new CustomEvent("ms-request-switch-view", { detail: { view: "settings" } }));
      };
      sItem.appendChild(sBtn);

      const sClose = document.createElement("button");
      sClose.type = "button";
      sClose.className = "tab-close";
      sClose.innerHTML = `<svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2"><path stroke-linecap="round" stroke-linejoin="round" d="M6 18 18 6M6 6l12 12"/></svg>`;
      sClose.title = "Close Settings tab";
      sClose.onclick = (e) => {
        e.stopPropagation();
        settingsTabOpen = false;
        if (currentPlatformView === "settings") {
          window.dispatchEvent(new CustomEvent("ms-request-switch-view", { detail: { view: "transcode" } }));
        } else {
          renderTabs();
        }
      };
      sItem.appendChild(sClose);
      strip.appendChild(sItem);
    }
  }

  function requestCloseTab(q, count, isBusy) {
    if (snapshot.queues.length <= 1) {
      return showToast("At least one workspace tab must remain open.", "info");
    }
    if (count === 0 && !isBusy) {
      invoke("delete_queue_tab", { queueId: q.id }).catch((e) => showToast(String(e), "warn"));
      return;
    }
    if (isBusy) {
      return showToast("Pause or cancel active work in this tab before closing it.", "warn");
    }
    openModal(
      `Delete "${q.name}" tab?`,
      `<p>This ${(MODE_META[activeMode(q)] || MODE_META.encode).fullLabel} tab holds <strong>${count}</strong> item${count === 1 ? "" : "s"}. Deleting the tab removes its workspace list only — your original videos on disk are never touched.</p>`,
      [
        { label: "Cancel", cls: "btn", onClick: closeModal },
        {
          label: "Delete Tab",
          cls: "btn btn-danger",
          onClick: async () => {
            closeModal();
            try {
              await invoke("delete_queue_tab", { queueId: q.id });
              showToast(`Deleted tab "${q.name}"`, "info");
            } catch (err) {
              showToast(String(err), "warn");
            }
          },
        },
      ]
    );
  }

  function renderActionBar() {
    const q = activeQueue();
    if (!q) return;
    const mode = activeMode(q);
    const jobs = activeQueueJobs();

    // Update mode switcher active state
    document.querySelectorAll("#tab-mode-switcher .tab-mode-btn").forEach((btn) => {
      const targetMode = btn.getAttribute("data-tab-mode");
      btn.classList.toggle("active", targetMode === mode);
    });

    // Show Preview 15s in Explore & Transcode when resources attached
    const prevBtn = $("btn-preview-15s");
    const showPreview = (mode === "explore" || mode === "encode") && jobs.length > 0;
    prevBtn.classList.toggle("hidden", !showPreview);

    // Forward button state + count
    const fwdPlan = getDefaultForwardPlan(q, jobs);
    const fwdBtn = $("btn-forward-tab");
    const fwdCount = $("forward-tab-count");
    fwdBtn.disabled = jobs.length === 0;
    if (fwdPlan.jobIds.length > 0) {
      fwdCount.textContent = String(fwdPlan.jobIds.length);
      fwdCount.classList.remove("hidden");
    } else {
      fwdCount.classList.add("hidden");
    }

    // Filter pills & Primary CTA per mode
    const pills = $("filter-pills-container");
    pills.innerHTML = "";
    const startBtn = $("btn-start-queue");
    const pauseBtn = $("btn-pause-after");
    const cancelBtn = $("btn-cancel-current");
    pauseBtn.classList.add("hidden");
    cancelBtn.classList.add("hidden");

    if (mode === "encode") {
      const selectable = (j) => isEncodeSelectableJob(j);
      const selectedReadyCount = jobs.filter((j) => checkedJobIds.has(j.id) && selectable(j) && j.plan?.is_ready).length;
      const selectedProbingCount = jobs.filter((j) => checkedJobIds.has(j.id) && selectable(j) && !j.plan).length;
      const tabQueued = jobs.filter(isQueuedJob);
      const activeOrWaitingCount = tabQueued.filter((j) => ["ready", "queued", "probing", "encoding", "verifying", "publishing"].includes(j.status)).length;
      const attCount = jobs.filter((j) => j.status === "needs_attention").length;
      const activeJob = tabQueued.find((j) => ["encoding", "verifying", "publishing"].includes(j.status));
      const inTabQueue = activeEncodeSubView(q) === "queue";

      const viewQueueBtn = $("btn-view-encode-queue");
      if (viewQueueBtn) {
        viewQueueBtn.classList.toggle("hidden", jobs.length === 0 && tabQueued.length === 0);
        viewQueueBtn.classList.toggle("active", inTabQueue);
        if (inTabQueue) {
          viewQueueBtn.textContent = "← Video List";
        } else {
          const cnt = activeOrWaitingCount > 0 ? activeOrWaitingCount : tabQueued.length;
          viewQueueBtn.textContent = `Queue (${cnt})`;
        }
      }

      if (attCount > 0) {
        const b = document.createElement("button");
        b.type = "button";
        b.className = "pill-filter-btn" + (encodeFilters.status === "needs_attention" ? " active" : "");
        b.textContent = `${attCount} need review`;
        b.onclick = () => {
          encodeFilters.status = encodeFilters.status === "needs_attention" ? "all" : "needs_attention";
          renderWorkspace();
          renderActionBar();
        };
        pills.appendChild(b);
      }

      if (activeJob) {
        pauseBtn.classList.remove("hidden");
        pauseBtn.textContent = q.pause_after_current ? "Pause scheduled" : "Pause after file";
        cancelBtn.classList.remove("hidden");
      }

      startBtn.classList.toggle("hidden", inTabQueue);
      startBtn.disabled = selectedReadyCount === 0;
      startBtn.textContent = selectedReadyCount > 0
        ? `Add ${selectedReadyCount} to Queue`
        : selectedProbingCount > 0
          ? `Waiting Stats (${selectedProbingCount})…`
          : "Select Videos to Queue";
    } else {
      startBtn.classList.remove("hidden");
      const prevViewBtn = $("btn-view-encode-queue");
      if (prevViewBtn) {
        prevViewBtn.classList.add("hidden");
        prevViewBtn.classList.remove("active");
      }
    }
    if (mode === "explore") {
      const compCount = jobs.filter((j) => ["compress_balanced", "compress_compact"].includes(jobDecision(j))).length;
      startBtn.disabled = compCount === 0;
      startBtn.textContent = compCount > 0 ? `Send ${compCount} to Transcode` : "Tag Videos to Compress";
    } else if (mode === "organize") {
      const trashCount = getOrgSelectedSet(q?.id).size;
      const isEmbedded = Boolean(EMBEDDED_ORG_PANELS[organizeSideTab]);
      startBtn.classList.toggle("hidden", isEmbedded);
      startBtn.disabled = jobs.length === 0 || dedupeScanning || ocrScanning || faceScanning;
      if (!isEmbedded) {
        if (organizeSideTab === "duplicates") {
          if (dedupeScanning) {
            startBtn.textContent = "Scanning…";
          } else if (trashCount > 0) {
            startBtn.textContent = `Delete ${trashCount} Selected`;
          } else {
            startBtn.textContent = "Scan Duplicates";
            startBtn.disabled = jobs.length < 2;
          }
        } else if (organizeSideTab === "watermarks") {
          startBtn.textContent = ocrScanning ? "Extracting…" : "Extract Watermarks";
        } else if (organizeSideTab === "faces") {
          startBtn.textContent = faceScanning ? "Grouping…" : "Detect People";
        } else if (organizeSideTab === "folder") {
          startBtn.textContent = "Preview Move/Copy";
        } else {
          startBtn.textContent = trashCount > 0 ? `Delete ${trashCount} Selected` : "Scan Duplicates";
        }
      }
    } else if (mode === "repair") {
      startBtn.classList.remove("hidden");
      const unrepaired = jobs.filter((j) => {
        const st = snapshot.tab_repair_items?.[j.id]?.status;
        return !["recovered", "partial_recovery"].includes(st);
      });
      const busy = jobs.some((j) => ["diagnosing", "recovering"].includes(snapshot.tab_repair_items?.[j.id]?.status));
      startBtn.disabled = jobs.length === 0 || busy || unrepaired.length === 0;
      startBtn.textContent = busy ? "Repairing…" : unrepaired.length > 0 ? `Repair ${unrepaired.length} Clip${unrepaired.length > 1 ? "s" : ""}` : "All Clips Repaired";
    }
  }

  const statsProgressByQueue = new Map(); // queueId -> { done, total, current_file }
  function renderLineageBar() {
    const q = activeQueue();
    const bar = $("resource-lineage-bar");
    const jobs = activeQueueJobs();
    if (!q || jobs.length === 0) {
      bar.classList.add("hidden");
      return;
    }
    bar.classList.remove("hidden");
    const meta = activeTabMeta(q);
    const totalBytes = jobs.reduce((a, b) => a + (b.input_bytes || 0), 0);
    const badgeHtml = meta.forwarded_from
      ? `<span class="badge" style="margin-right:6px;">← Forwarded from ${escapeHtml(meta.forwarded_from)}</span>`
      : "";
    $("resource-lineage-text").innerHTML = `${badgeHtml}<span>${escapeHtml(meta.resource_source || `${jobs.length} attached resource${jobs.length === 1 ? "" : "s"}`)}</span>`;
    const sp = statsProgressByQueue.get(q.id);
    const statsLabel = sp && sp.done < sp.total
      ? `Analyzing stats ${sp.done}/${sp.total}… · `
      : "";
    $("resource-lineage-meta").textContent = `${statsLabel}${(MODE_META[activeMode(q)] || MODE_META.encode).fullLabel} · ${jobs.length} items · ${fmtBytes(totalBytes)}`;
  }

  function renderWorkspace() {
    const q = activeQueue();
    if (!q) return;
    const mode = activeMode(q);
    const jobs = activeQueueJobs();

    const views = {
      explore: $("mode-view-explore"),
      encode: $("mode-view-encode"),
      organize: $("mode-view-organize"),
      repair: $("mode-view-repair"),
    };
    Object.values(views).forEach((el) => el.classList.add("hidden"));
    $("forward-sticky-bar").classList.add("hidden");

    if (jobs.length === 0 && mode !== "organize") {
      $("empty-drop-state").classList.remove("hidden");
      renderEmptyState(q, mode);
      return;
    }
    $("empty-drop-state").classList.add("hidden");

    if (mode === "explore") {
      views.explore.classList.remove("hidden");
      renderExploreView(q, jobs);
    } else if (mode === "organize") {
      views.organize.classList.remove("hidden");
      renderOrganizeView(q, jobs);
    } else if (mode === "repair") {
      views.repair.classList.remove("hidden");
      renderRepairView(q, jobs);
    } else {
      views.encode.classList.remove("hidden");
      const listView = $("tab-encode-list-view");
      const queueView = $("tab-encode-queue-view");
      if (activeEncodeSubView(q) === "queue") {
        listView?.classList.add("hidden");
        queueView?.classList.remove("hidden");
        renderTabQueueView(q, jobs);
      } else {
        queueView?.classList.add("hidden");
        listView?.classList.remove("hidden");
        renderBanners(q, jobs);
        renderQueueList(q, jobs);
      }
    }
  }

  function renderEmptyState(q, mode) {
    const meta = MODE_META[mode] || MODE_META.encode;
    $("empty-drop-title").textContent = `Attach videos or a folder to "${q.name}"`;
    $("empty-defaults-summary").textContent = `Active mode: ${meta.fullLabel} — ${meta.desc}`;

    const picker = $("empty-mode-picker");
    picker.innerHTML = Object.entries(MODE_META).map(([key, m]) => `
      <button type="button" data-empty-mode="${key}" class="empty-mode-card ${key === mode ? "active" : ""}">
        <div class="inline-row" style="gap:6px;font-size:12px;font-weight:600;">${m.icon}<span>${m.label}</span></div>
        <div class="muted" style="font-size:10.5px;margin-top:4px;line-height:1.35;">${escapeHtml(m.desc)}</div>
      </button>
    `).join("");

    picker.querySelectorAll("[data-empty-mode]").forEach((btn) => {
      btn.addEventListener("click", async () => {
        const nextMode = btn.getAttribute("data-empty-mode");
        await invoke("set_queue_tab_type", { queueId: q.id, tabType: nextMode });
      });
    });
  }

  // =========================================================================
  // 4. MODE VIEWS: EXPLORE | TRANSCODE | ORGANIZE | REPAIR
  // =========================================================================
  function renderExploreView(q, jobs) {
    const el = $("mode-view-explore");
    const counts = {
      all: jobs.length,
      unreviewed: jobs.filter((j) => jobDecision(j) === "unreviewed").length,
      compress: jobs.filter((j) => ["compress_balanced", "compress_compact"].includes(jobDecision(j))).length,
      protected: jobs.filter((j) => jobDecision(j) === "protected").length,
      repair: jobs.filter((j) => jobDecision(j) === "repair").length,
    };

    let visible = [...jobs];
    if (exploreFilterMode === "unreviewed") visible = visible.filter((j) => jobDecision(j) === "unreviewed");
    else if (exploreFilterMode === "compress") visible = visible.filter((j) => ["compress_balanced", "compress_compact"].includes(jobDecision(j)));
    else if (exploreFilterMode === "protected") visible = visible.filter((j) => jobDecision(j) === "protected");
    else if (exploreFilterMode === "repair") visible = visible.filter((j) => jobDecision(j) === "repair");

    if (sortMode === "bitrate") visible.sort((a, b) => jobVideoInfo(b).br - jobVideoInfo(a).br);
    else visible.sort((a, b) => b.input_bytes - a.input_bytes);

    const maxBytes = Math.max(1, ...jobs.map((j) => j.input_bytes || 0));

    el.innerHTML = `
      <div class="explore-filter-bar">
        <div class="inline-row" style="flex-wrap:wrap;">
          ${[
            ["all", `All (${counts.all})`],
            ["unreviewed", `Unreviewed (${counts.unreviewed})`],
            ["compress", `To Compress (${counts.compress})`],
            ["protected", `Protected ★ (${counts.protected})`],
            ["repair", `Repair (${counts.repair})`],
          ].map(([k, lbl]) => `<button type="button" data-exp-flt="${k}" class="pill-filter-btn ${exploreFilterMode === k ? "active" : ""}">${lbl}</button>`).join("")}
        </div>
        <label class="inline-row muted" style="font-size:12px;">
          <span>Sort:</span>
          <select id="sel-explore-sort" class="compact-sel">
            <option value="largest" ${sortMode !== "bitrate" ? "selected" : ""}>Largest First (Reclaim Max GB)</option>
            <option value="bitrate" ${sortMode === "bitrate" ? "selected" : ""}>Highest Bitrate First</option>
          </select>
        </label>
      </div>
      <div id="explore-rows-list" class="sidebar-panel-stack"></div>
    `;

    el.querySelectorAll("[data-exp-flt]").forEach((b) => {
      b.onclick = () => {
        exploreFilterMode = b.getAttribute("data-exp-flt");
        renderExploreView(q, jobs);
      };
    });
    el.querySelector("#sel-explore-sort").onchange = (e) => {
      sortMode = e.target.value;
      renderExploreView(q, jobs);
    };

    const list = el.querySelector("#explore-rows-list");
    for (const j of visible) {
      const dec = jobDecision(j);
      const info = jobVideoInfo(j);
      const pct = Math.max(4, Math.round(((j.input_bytes || 0) / maxBytes) * 100));
      const row = document.createElement("article");
      const decCls = dec === "protected" ? "dec-protected" : dec.startsWith("compress") ? "dec-compress" : dec === "repair" ? "dec-repair" : "";
      row.className = `explore-row ${decCls} ${j.id === selectedJobId ? "selected" : ""}`;

      let badgeHtml = `<span class="badge">Unreviewed</span>`;
      if (dec === "protected") badgeHtml = `<span class="badge badge-warn">★ Protected · Keep Original</span>`;
      else if (dec === "compress_balanced") badgeHtml = `<span class="badge badge-ok">Compress · Balanced HEVC</span>`;
      else if (dec === "compress_compact") badgeHtml = `<span class="badge badge-ok">Compress · Compact AV1</span>`;
      else if (dec === "repair") badgeHtml = `<span class="badge badge-err">Needs Repair</span>`;

      row.innerHTML = `
        <div class="explore-size-fill" style="width:${pct}%"></div>
        <div class="explore-row-inner">
          <div class="min-w-0" style="flex:1;">
            <div class="inline-row" style="flex-wrap:wrap;gap:6px;">
              <strong style="font-size:13px;">${escapeHtml(jobFileName(j))}</strong>
              ${badgeHtml}
            </div>
            <div class="muted tabular-nums" style="font-size:11.5px;margin-top:3px;">
              ${escapeHtml(info.codec)} · ${escapeHtml(info.res)} · ${info.mbps > 0 ? info.mbps + " Mbps" : "—"} · ${fmtDuration(info.dur)} · <span class="mono">${escapeHtml(j.source_rel_path)}</span>
            </div>
          </div>
          <div class="inline-row" style="gap:10px;">
            <div style="text-align:right;" class="tabular-nums">
              <div style="font-size:13px;font-weight:700;">${fmtBytes(j.input_bytes)}</div>
              <div class="muted" style="font-size:10.5px;">${pct}% of largest</div>
            </div>
            <div class="triage-group">
              <button type="button" class="triage-btn ${dec === "protected" ? "active-protect" : ""}" data-tag="${j.id}:protected">★ Keep</button>
              <button type="button" class="triage-btn ${dec === "compress_balanced" ? "active-balanced" : ""}" data-tag="${j.id}:compress_balanced">Balanced</button>
              <button type="button" class="triage-btn ${dec === "compress_compact" ? "active-compact" : ""}" data-tag="${j.id}:compress_compact">Compact</button>
              <button type="button" class="triage-btn ${dec === "repair" ? "active-repair" : ""}" data-tag="${j.id}:repair">Repair</button>
            </div>
            <button type="button" class="btn btn-ghost btn-sm" data-rm="${j.id}" title="Remove from tab">×</button>
          </div>
        </div>
      `;

      row.addEventListener("click", (e) => {
        if (e.target.closest("button")) return;
        selectedJobId = j.id;
        sidebarMode = "job";
        sidebarOpen = true;
        renderAll();
      });
      row.querySelectorAll("[data-tag]").forEach((b) => {
        b.onclick = async (e) => {
          e.stopPropagation();
          const [jid, val] = b.getAttribute("data-tag").split(":");
          const next = dec === val ? "unreviewed" : val;
          selectedJobId = jid;
          await invoke("set_job_decision", { jobId: jid, decision: next });
        };
      });
      row.querySelector("[data-rm]").onclick = (e) => {
        e.stopPropagation();
        invoke("remove_pending_jobs", { jobIds: [j.id] });
      };
      list.appendChild(row);
    }

    // Sticky pipeline handoff bar
    const compressItems = jobs.filter((j) => ["compress_balanced", "compress_compact"].includes(jobDecision(j)));
    const repairItems = jobs.filter((j) => jobDecision(j) === "repair");
    if (compressItems.length > 0 || repairItems.length > 0) {
      const bar = $("forward-sticky-bar");
      bar.classList.remove("hidden");
      const compBytes = compressItems.reduce((a, b) => a + b.input_bytes, 0);
      bar.innerHTML = `
        <div class="tabular-nums" style="font-size:12px;">
          <strong>Ready to forward:</strong>
          ${compressItems.length > 0 ? `<span style="color:var(--ok);margin-left:8px;">${compressItems.length} to Compress (${fmtBytes(compBytes)})</span>` : ""}
          ${repairItems.length > 0 ? `<span style="color:var(--danger);margin-left:8px;">${repairItems.length} to Repair</span>` : ""}
        </div>
        <div class="inline-row">
          ${repairItems.length > 0 ? `<button type="button" id="stk-fwd-repair" class="btn btn-sm btn-danger-outline">Forward ${repairItems.length} → Repair Tab</button>` : ""}
          ${compressItems.length > 0 ? `<button type="button" id="stk-fwd-transcode" class="btn btn-sm btn-primary">Forward ${compressItems.length} → Transcode Tab</button>` : ""}
        </div>
      `;
      bar.querySelector("#stk-fwd-transcode")?.addEventListener("click", () => {
        openForwardModal(compressItems.map((j) => j.id), "encode", "clips tagged for compression", false);
      });
      bar.querySelector("#stk-fwd-repair")?.addEventListener("click", () => {
        openForwardModal(repairItems.map((j) => j.id), "repair", "damaged clips for repair", false);
      });
    }
  }

  function renderBanners(q, jobs) {
    const area = $("banner-area");
    area.innerHTML = "";
    const ms = snapshot.morning_summary;
    const activeSummary = ms && ms.queue_id === q.id ? ms : null;
    const completed = jobs.filter((j) => j.status === "completed");

    if (activeSummary && completed.length > 0 && activeSummary.total_input_bytes > 0) {
      const savedBytes = Math.max(0, activeSummary.total_input_bytes - activeSummary.total_output_bytes);
      const pct = Math.round((1 - activeSummary.total_output_bytes / activeSummary.total_input_bytes) * 100);
      const b = document.createElement("div");
      b.className = "banner-card ok";
      b.innerHTML = `
        <div>
          <div class="inline-row" style="gap:8px;flex-wrap:wrap;">
            <strong>${fmtBytes(activeSummary.total_input_bytes)} input → ${fmtBytes(activeSummary.total_output_bytes)} output</strong>
            <span class="badge badge-ok">${pct}% smaller · ${fmtBytes(savedBytes)} saved</span>
          </div>
          <div class="muted tabular-nums" style="font-size:12px;margin-top:2px;">
            ${activeSummary.completed_count} verified · ${activeSummary.kept_count} originals kept · ${activeSummary.trashed_count} moved to Trash
          </div>
        </div>
        <div class="inline-row">
          <button type="button" id="bn-show-output" class="btn btn-sm btn-secondary">Show Output</button>
          <button type="button" id="bn-review-originals" class="btn btn-sm btn-primary">Review Originals…</button>
        </div>
      `;
      b.querySelector("#bn-show-output").onclick = () => invoke("reveal_output", { path: completed[0].destination_path });
      b.querySelector("#bn-review-originals").onclick = () => openReviewOriginalsModal(q.id);
      area.appendChild(b);
    }

    const otherRunning = snapshot.queues.find((oq) =>
      oq.id !== q.id && oq.is_running && !oq.is_paused && snapshot.jobs.some((j) => j.queue_id === oq.id && ["encoding", "verifying"].includes(j.status))
    );
    if (q.waiting_reason || (otherRunning && q.is_running && !q.is_paused)) {
      const w = document.createElement("div");
      w.className = "banner-card warn";
      const txt = q.waiting_reason || `Waiting: "${otherRunning.name}" is using the single-encoder engine.`;
      w.innerHTML = `
        <span style="font-size:12px;">${escapeHtml(txt)}</span>
        ${otherRunning ? `<button type="button" id="bn-jump-active" class="btn-link">View ${escapeHtml(otherRunning.name)} →</button>` : ""}
      `;
      w.querySelector("#bn-jump-active")?.addEventListener("click", () => invoke("select_queue_tab", { queueId: otherRunning.id }));
      area.appendChild(w);
    }
  }

  function jobStatusIconHtml(status) {
    if (["encoding", "verifying", "publishing"].includes(status)) {
      return `<span class="job-status-icon running"><svg class="ms-spinner" width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2"><path d="M21 12a9 9 0 1 1-6.219-8.56" stroke-linecap="round"/></svg></span>`;
    }
    if (status === "completed") {
      return `<span class="job-status-icon done"><svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.3" stroke-linecap="round" stroke-linejoin="round"><path d="M22 11.08V12a10 10 0 1 1-5.93-9.14"/><polyline points="22 4 12 14.01 9 11.01"/></svg></span>`;
    }
    if (["failed", "needs_attention"].includes(status)) {
      return `<span class="job-status-icon failed"><svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><line x1="12" y1="8" x2="12" y2="12"/><line x1="12" y1="16" x2="12.01" y2="16"/></svg></span>`;
    }
    if (["interrupted", "canceled"].includes(status)) {
      return `<span class="job-status-icon paused"><svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><line x1="10" y1="15" x2="10" y2="9"/><line x1="14" y1="15" x2="14" y2="9"/></svg></span>`;
    }
    return `<span class="job-status-icon"><svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="9"/><polyline points="12 7 12 12 15 14"/></svg></span>`;
  }

  function isModernCodec(codecUpper) {
    return ["HEVC", "H265", "AV1"].includes((codecUpper || "").toUpperCase());
  }

  function isQueuedJob(j) {
    if (!j) return false;
    if (["queued", "probing", "ready", "encoding", "verifying", "publishing", "completed", "below_savings_target", "failed", "interrupted"].includes(j.status)) {
      return true;
    }
    if (j.status === "canceled" && j.status_reason !== "Not selected") return true;
    if (j.status === "needs_attention" && ["encoding", "verifying", "publishing"].includes(j.error_stage)) {
      return true;
    }
    return false;
  }

  function isEncodeSelectableJob(j) {
    return !isQueuedJob(j) && ["listed", "canceled"].includes(j.status) && !j.error_message;
  }

  function renderQueueBadges() {
    // Queue badges now render per-tab in the tab strip and in the active tab's action bar.
  }

  function closeColFilterPopover() {
    if (activeColPopover) {
      activeColPopover.remove();
      activeColPopover = null;
    }
  }

  function resetAllEncodeFilters() {
    encodeFilters.search = "";
    encodeFilters.searchMode = "name";
    encodeFilters.searchInvert = false;
    encodeFilters.codec = "all";
    encodeFilters.selectedCodecs.clear();
    encodeFilters.resOp = "ge";
    encodeFilters.resHeight = 0;
    encodeFilters.durOp = "ge";
    encodeFilters.durSecs = 0;
    encodeFilters.sizeOp = "ge";
    encodeFilters.minMb = 0;
    encodeFilters.bitrateOp = "ge";
    encodeFilters.minMbps = 0;
    encodeFilters.target = "all";
    encodeFilters.status = "all";
    if ($("enc-search-input")) $("enc-search-input").value = "";
  }

  function canonicalCodecLabel(j) {
    const raw = String(jobVideoInfo(j).codec || "").toUpperCase();
    if (!raw || raw === "VIDEO" || raw === "STATS…" || raw === "—") return "";
    if (["H264", "AVC", "AVC1"].includes(raw)) return "H.264";
    if (["HEVC", "H265", "HVC1", "HEV1"].includes(raw)) return "HEVC";
    if (["AV1", "AV01"].includes(raw)) return "AV1";
    if (["PRORES", "APCH", "APCN", "APCS", "APCO", "AP4H", "AP4X"].includes(raw)) return "ProRes";
    return raw;
  }

  function compileEncodeSearchMatcher() {
    const raw = encodeFilters.search.trim();
    if (!raw) return { active: false, valid: true, error: null, test: () => true };
    const mode = encodeFilters.searchMode || "name";
    const inv = !!encodeFilters.searchInvert;
    if (mode === "regex") {
      try {
        const re = new RegExp(raw, "i");
        return {
          active: true,
          valid: true,
          error: null,
          test: (j) => {
            const hay = `${j.source_path || ""} ${j.source_rel_path || ""} ${jobFileName(j)}`;
            const matched = re.test(hay);
            return inv ? !matched : matched;
          },
        };
      } catch (err) {
        return { active: true, valid: false, error: err.message || "Invalid regex", test: () => false };
      }
    }
    const qLower = raw.toLowerCase();
    return {
      active: true,
      valid: true,
      error: null,
      test: (j) => {
        const hay = mode === "path"
          ? `${j.source_path || ""} ${j.source_rel_path || ""}`.toLowerCase()
          : jobFileName(j).toLowerCase();
        const matched = hay.includes(qLower);
        return inv ? !matched : matched;
      },
    };
  }

  function encodeFilterColumnMatch(j, searchMatcher = null) {
    const info = jobVideoInfo(j);
    const sm = searchMatcher || compileEncodeSearchMatcher();
    if (sm.active && !sm.test(j)) return false;
    const codec = (info.codec || "").toUpperCase();
    if (encodeFilters.selectedCodecs.size > 0) {
      const canon = canonicalCodecLabel(j);
      if (!canon || !encodeFilters.selectedCodecs.has(canon)) return false;
    } else {
      if (encodeFilters.codec === "non_modern" && (!j.probe || isModernCodec(codec))) return false;
      if (encodeFilters.codec === "only_h264" && !["H264", "AVC", "AVC1"].includes(codec)) return false;
      if (encodeFilters.codec === "only_legacy" && (isModernCodec(codec) || ["H264", "AVC", "AVC1", "VIDEO", "STATS…"].includes(codec))) return false;
      if (encodeFilters.codec === "modern_only" && !isModernCodec(codec)) return false;
    }

    const h = info.v?.height || 0;
    const w = info.v?.width || 0;
    if (encodeFilters.resHeight > 0) {
      if (h === 0 && w === 0) return false;
      const effH = Math.max(Math.min(w, h), Math.round(Math.max(w, h) * 9 / 16));
      if (encodeFilters.resOp === "lt") {
        if (effH >= encodeFilters.resHeight) return false;
      } else if (effH < encodeFilters.resHeight) {
        return false;
      }
    }

    const dur = info.dur || 0;
    if (encodeFilters.durSecs > 0) {
      if (dur <= 0) return false;
      if (encodeFilters.durOp === "lt") {
        if (dur >= encodeFilters.durSecs) return false;
      } else if (dur < encodeFilters.durSecs) {
        return false;
      }
    }

    if (encodeFilters.minMb > 0) {
      const threshBytes = encodeFilters.minMb * 1024 * 1024;
      const sz = j.input_bytes || 0;
      if (encodeFilters.sizeOp === "lt") {
        if (sz >= threshBytes) return false;
      } else if (sz < threshBytes) {
        return false;
      }
    }

    if (encodeFilters.minMbps > 0) {
      const mbpsVal = info.mbps || 0;
      if (mbpsVal <= 0) return false;
      if (encodeFilters.bitrateOp === "lt") {
        if (mbpsVal >= encodeFilters.minMbps) return false;
      } else if (mbpsVal < encodeFilters.minMbps) {
        return false;
      }
    }

    const targetKind = j.recipe.family === "remux" ? "remux" : String(j.recipe.codec || "").toLowerCase();
    if (encodeFilters.target !== "all" && targetKind !== encodeFilters.target) return false;

    if (encodeFilters.status === "listed" && !(!isQueuedJob(j) && j.status === "listed")) return false;
    if (encodeFilters.status === "selected" && !(isEncodeSelectableJob(j) && checkedJobIds.has(j.id))) return false;
    if (encodeFilters.status === "ready" && !["queued", "ready"].includes(j.status)) return false;
    if (encodeFilters.status === "completed" && j.status !== "completed") return false;
    if (encodeFilters.status === "needs_attention" && j.status !== "needs_attention") return false;
    if (encodeFilters.status === "failed" && !["failed", "interrupted", "canceled"].includes(j.status)) return false;

    return true;
  }

  function sortEncodeJobs(list) {
    if (!encodeSort.col) return list;
    const mul = encodeSort.dir === "asc" ? 1 : -1;
    const cmpText = (a, b) => a.localeCompare(b, undefined, { numeric: true, sensitivity: "base" }) * mul;
    const cmpNum = (a, b) => (a - b) * mul;
    return list.sort((a, b) => {
      const ia = jobVideoInfo(a);
      const ib = jobVideoInfo(b);
      switch (encodeSort.col) {
        case "file":
          return cmpText(jobFileName(a), jobFileName(b));
        case "codec":
          return cmpText(ia.codec || "", ib.codec || "");
        case "res": {
          const pxA = (ia.v?.width || 0) * (ia.v?.height || 0);
          const pxB = (ib.v?.width || 0) * (ib.v?.height || 0);
          return cmpNum(pxA, pxB);
        }
        case "duration":
          return cmpNum(ia.dur || 0, ib.dur || 0);
        case "size":
          return cmpNum(a.input_bytes || 0, b.input_bytes || 0);
        case "bitrate":
          return cmpNum(ia.br || 0, ib.br || 0);
        case "target": {
          const ta = a.recipe.family === "remux" ? `remux ${a.recipe.container}` : `${a.recipe.codec} ${a.recipe.crf}`;
          const tb = b.recipe.family === "remux" ? `remux ${b.recipe.container}` : `${b.recipe.codec} ${b.recipe.crf}`;
          return cmpText(ta, tb);
        }
        case "status":
          return cmpText(a.status || "", b.status || "");
        default:
          return 0;
      }
    });
  }

  const SIZE_FILTER_PRESETS = [0, 100, 500, 1024, 2048, 5120];
  const DUR_FILTER_PRESETS = [0, 60, 300, 900, 3600];
  const BITRATE_FILTER_PRESETS = [0, 5, 10, 20, 50];
  const RES_FILTER_PRESETS = [0, 480, 720, 1080, 1440, 2160];

  function opSym(op) {
    return op === "lt" ? "<" : "≥";
  }
  function formatResFilterLabel(op, h) {
    if (!h || h <= 0) return "Any";
    const name = h === 2160 ? "4K (2160p)" : `${h}p`;
    return `${opSym(op)} ${name}`;
  }
  function formatDurFilterLabel(op, secs) {
    if (!secs || secs <= 0) return "Any";
    let txt;
    if (secs >= 3600 && secs % 60 === 0) txt = `${+(secs / 3600).toFixed(2)}h`;
    else if (secs >= 60) txt = `${+(secs / 60).toFixed(1)}m`;
    else txt = `${+Number(secs).toFixed(1)}s`;
    return `${opSym(op)} ${txt}`;
  }
  function formatMinSizeLabel(minMb, op = "ge") {
    if (!minMb || minMb <= 0) return "Any Size";
    if (minMb >= 1024) {
      const gb = +(minMb / 1024).toFixed(2);
      return `${opSym(op)} ${gb} GB`;
    }
    const mb = +Number(minMb).toFixed(1);
    return `${opSym(op)} ${mb} MB`;
  }
  function formatBitrateFilterLabel(op, mbps) {
    if (!mbps || mbps <= 0) return "Any";
    return `${opSym(op)} ${+Number(mbps).toFixed(1)} Mbps`;
  }

  function syncEncodeFilterControls(searchMatcher = null) {
    const sm = searchMatcher || compileEncodeSearchMatcher();
    const inp = $("enc-search-input");
    if (inp) {
      if (document.activeElement !== inp) inp.value = encodeFilters.search;
      const ph = {
        name: "Search file name…",
        path: "Search full file path…",
        regex: "Regex pattern (e.g. s01e\\d+.*1080p)…",
      };
      inp.placeholder = ph[encodeFilters.searchMode] || ph.name;
      inp.classList.toggle("has-error", !sm.valid);
      inp.title = sm.error ? `Invalid regex: ${sm.error}` : "";
    }
    $("enc-search-mode-seg")?.querySelectorAll("[data-search-mode]").forEach((b) => {
      b.classList.toggle("active", b.getAttribute("data-search-mode") === encodeFilters.searchMode);
    });

    const chips = [];
    if (encodeFilters.search.trim()) {
      const modeLbl = encodeFilters.searchMode === "path" ? "Path" : encodeFilters.searchMode === "regex" ? "Regex" : "Name";
      const opLbl = encodeFilters.searchInvert ? " ≠ " : ": ";
      const errTag = !sm.valid ? " (invalid regex)" : "";
      chips.push({ key: "search", col: "file", label: `${modeLbl}${opLbl}"${encodeFilters.search.trim()}"${errTag}` });
    }
    if (encodeFilters.selectedCodecs.size > 0) {
      chips.push({ key: "selectedCodecs", col: "codec", label: `Codec: ${Array.from(encodeFilters.selectedCodecs).join(", ")}` });
    } else if (encodeFilters.codec !== "all") {
      const m = { non_modern: "Exclude HEVC & AV1", only_h264: "Only H.264", only_legacy: "Only Legacy", modern_only: "HEVC / AV1 only" };
      chips.push({ key: "codec", col: "codec", label: `Codec: ${m[encodeFilters.codec] || encodeFilters.codec}` });
    }
    if (encodeFilters.resHeight > 0) {
      chips.push({ key: "resHeight", col: "res", label: `Res: ${formatResFilterLabel(encodeFilters.resOp, encodeFilters.resHeight)}` });
    }
    if (encodeFilters.durSecs > 0) {
      chips.push({ key: "durSecs", col: "duration", label: `Duration: ${formatDurFilterLabel(encodeFilters.durOp, encodeFilters.durSecs)}` });
    }
    if (encodeFilters.minMb > 0) {
      chips.push({ key: "minMb", col: "size", label: `Size: ${formatMinSizeLabel(encodeFilters.minMb, encodeFilters.sizeOp)}` });
    }
    if (encodeFilters.minMbps > 0) {
      chips.push({ key: "minMbps", col: "bitrate", label: `Bitrate: ${formatBitrateFilterLabel(encodeFilters.bitrateOp, encodeFilters.minMbps)}` });
    }
    if (encodeFilters.target !== "all") {
      chips.push({ key: "target", col: "target", label: `Target: ${encodeFilters.target.toUpperCase()}` });
    }
    if (encodeFilters.status !== "all") {
      const m = { listed: "Not Yet Queued", selected: "Checked", ready: "In Queue", completed: "Completed", needs_attention: "Needs Attention", failed: "Failed / Interrupted" };
      chips.push({ key: "status", col: "status", label: `Status: ${m[encodeFilters.status] || encodeFilters.status}` });
    }

    const chipsEl = $("enc-active-chips");
    if (chipsEl) {
      chipsEl.innerHTML = chips.map((c) => `
        <span class="enc-chip">
          <button type="button" class="enc-chip-edit" data-edit-chip="${c.col}" title="Click to change filter">
            <span>${escapeHtml(c.label)}</span>
            <span class="enc-chip-caret" aria-hidden="true">▾</span>
          </button>
          <button type="button" class="enc-chip-clear" data-clear-chip="${c.key}" title="Remove filter">×</button>
        </span>
      `).join("") + (chips.length > 1 ? `<button type="button" id="enc-chips-clear-all" class="btn btn-sm btn-ghost" style="height:24px;padding:0 8px;font-size:11px;">Clear all</button>` : "");
      chipsEl.querySelectorAll("[data-edit-chip]").forEach((btn) => {
        btn.onclick = (e) => {
          e.stopPropagation();
          const col = btn.getAttribute("data-edit-chip");
          if (activeColPopover && activeColPopover.dataset?.chipCol === col) {
            closeColFilterPopover();
            return;
          }
          openColFilterPopover(col, btn);
          if (activeColPopover) activeColPopover.dataset.chipCol = col;
        };
      });
      chipsEl.querySelectorAll("[data-clear-chip]").forEach((btn) => {
        btn.onclick = (e) => {
          e.stopPropagation();
          closeColFilterPopover();
          const k = btn.getAttribute("data-clear-chip");
          if (k === "search") {
            encodeFilters.search = "";
            encodeFilters.searchInvert = false;
          } else if (k === "selectedCodecs") {
            encodeFilters.selectedCodecs.clear();
          } else if (k === "resHeight") {
            encodeFilters.resHeight = 0;
          } else if (k === "durSecs") {
            encodeFilters.durSecs = 0;
          } else if (k === "minMb") {
            encodeFilters.minMb = 0;
          } else if (k === "minMbps") {
            encodeFilters.minMbps = 0;
          } else encodeFilters[k] = "all";
          renderQueueList(activeQueue(), activeQueueJobs());
          renderActionBar();
        };
      });
      chipsEl.querySelector("#enc-chips-clear-all")?.addEventListener("click", () => {
        closeColFilterPopover();
        resetAllEncodeFilters();
        renderQueueList(activeQueue(), activeQueueJobs());
        renderActionBar();
      });
    }

    const moreBtn = $("btn-more-filters");
    if (moreBtn) {
      const nonSearchCount = chips.filter((c) => c.key !== "search").length;
      const isOpen = !$("encode-more-filter-drawer")?.classList.contains("hidden");
      moreBtn.textContent = nonSearchCount > 0 ? `Filters (${nonSearchCount}) ${isOpen ? "▴" : "▾"}` : `Filters ${isOpen ? "▴" : "▾"}`;
      moreBtn.classList.toggle("active", isOpen || nonSearchCount > 0);
    }
    const resetSortBtn = $("btn-reset-col-sort");
    if (resetSortBtn) resetSortBtn.classList.toggle("hidden", !encodeSort.col);
  }

  function renderFilterRulesDrawer(allJobs = activeQueueJobs(), matchedJobs = null) {
    const dr = $("encode-more-filter-drawer");
    if (!dr || dr.classList.contains("hidden")) return;

    const sm = compileEncodeSearchMatcher();
    const matched = matchedJobs || allJobs.filter((j) => encodeFilterColumnMatch(j, sm));
    const selectableMatches = matched.filter(isEncodeSelectableJob);
    const matchBytes = selectableMatches.reduce((a, b) => a + (b.input_bytes || 0), 0);

    // Build dynamic codec counts in current tab
    const codecCounts = new Map([["H.264", 0], ["AV1", 0], ["HEVC", 0], ["ProRes", 0]]);
    for (const j of allJobs) {
      const c = canonicalCodecLabel(j);
      if (!c) continue;
      codecCounts.set(c, (codecCounts.get(c) || 0) + 1);
    }
    const codecEntries = Array.from(codecCounts.entries()).filter(([name, cnt]) =>
      cnt > 0 || ["H.264", "AV1", "HEVC", "ProRes"].includes(name)
    );

    const bestSavingsActive = encodeFilters.codec === "non_modern" && encodeFilters.selectedCodecs.size === 0 && encodeFilters.sizeOp === "ge" && encodeFilters.minMb === 1024;
    const exclModernActive = encodeFilters.codec === "non_modern" && encodeFilters.selectedCodecs.size === 0;
    const bitrate20Active = encodeFilters.bitrateOp === "ge" && encodeFilters.minMbps === 20;
    const modernOnlyActive = encodeFilters.codec === "modern_only" && encodeFilters.selectedCodecs.size === 0;

    // Preserve active numeric input if user is currently typing inside the drawer
    const activeId = document.activeElement && dr.contains(document.activeElement) ? document.activeElement.id : null;
    if (activeId && ["fr-size-val", "fr-mbps-val", "fr-res-val", "fr-dur-val"].includes(activeId)) {
      const statEl = dr.querySelector("#fr-match-stat");
      if (statEl) {
        statEl.innerHTML = `<span class="fr-match-dot"></span><span><strong>${selectableMatches.length}</strong> match${selectableMatches.length === 1 ? "" : "es"} (${fmtBytes(matchBytes)})</span>` +
          (matched.length !== selectableMatches.length ? `<span class="muted">· ${matched.length} total</span>` : "");
      }
      const selBtn = dr.querySelector("#fr-act-select");
      if (selBtn) {
        selBtn.textContent = `Select ${selectableMatches.length}`;
        selBtn.disabled = selectableMatches.length === 0;
      }
      const addBtn = dr.querySelector("#fr-act-add");
      if (addBtn) addBtn.disabled = selectableMatches.length === 0;
      const deBtn = dr.querySelector("#fr-act-deselect");
      if (deBtn) deBtn.disabled = selectableMatches.length === 0;
      return;
    }

    const sizeUnit = encodeFilters.sizeUnit === "MB" ? "MB" : "GB";
    const sizeValStr = encodeFilters.minMb > 0
      ? String(+(encodeFilters.minMb / (sizeUnit === "GB" ? 1024 : 1)).toFixed(2))
      : "";
    const mbpsValStr = encodeFilters.minMbps > 0 ? String(+Number(encodeFilters.minMbps).toFixed(1)) : "";
    const resValStr = encodeFilters.resHeight > 0 ? String(encodeFilters.resHeight) : "";
    const durUnit = ["s", "m", "h"].includes(encodeFilters.durUnit) ? encodeFilters.durUnit : "m";
    const durMult = durUnit === "h" ? 3600 : durUnit === "s" ? 1 : 60;
    const durValStr = encodeFilters.durSecs > 0 ? String(+(encodeFilters.durSecs / durMult).toFixed(2)) : "";

    dr.innerHTML = `
      <div class="fr-head">
        <span class="fr-title">Filter &amp; Smart Selection Rules</span>
        <div class="fr-head-actions">
          <button type="button" id="fr-clear-all" class="fr-link-btn">Clear all</button>
          <button type="button" id="fr-close" class="fr-close-btn" title="Close filter rules">×</button>
        </div>
      </div>

      <div class="fr-smart-row">
        <button type="button" class="fr-smart-pill ${bestSavingsActive ? "active" : ""}" data-fr-smart="best_savings">Best Savings (≥1 GB)</button>
        <button type="button" class="fr-smart-pill ${exclModernActive ? "active" : ""}" data-fr-smart="non_modern">Exclude HEVC &amp; AV1</button>
        <button type="button" class="fr-smart-pill ${bitrate20Active ? "active" : ""}" data-fr-smart="bitrate_20">Bitrate ≥20 Mbps</button>
        <button type="button" class="fr-smart-pill is-amber ${modernOnlyActive ? "active" : ""}" data-fr-smart="modern_only">HEVC / AV1 Only</button>
      </div>

      <div class="fr-divider"></div>

      <div>
        <div class="fr-sec-head">
          <span class="fr-sec-label">Encodings in Queue (Multi-Select)</span>
          <div class="fr-quick-links">
            <button type="button" class="fr-qlink ${encodeFilters.selectedCodecs.size === 0 && encodeFilters.codec === "all" ? "active" : ""}" data-fr-codec-quick="all">Any</button>
            <span>·</span>
            <button type="button" class="fr-qlink ${exclModernActive ? "active" : ""}" data-fr-codec-quick="non_modern">Non-HEVC/AV1</button>
          </div>
        </div>
        <div class="fr-codec-grid">
          ${codecEntries.map(([name, cnt]) => {
            const active = encodeFilters.selectedCodecs.has(name);
            return `
              <button type="button" class="fr-codec-chip ${active ? "active" : ""}" data-fr-codec="${escapeHtml(name)}">
                <span class="fr-chk-box" aria-hidden="true">${active ? "✓" : ""}</span>
                <span>${escapeHtml(name)}</span>
                <span class="fr-codec-cnt">(${cnt})</span>
              </button>
            `;
          }).join("")}
        </div>
      </div>

      <div class="fr-divider"></div>

      <div class="fr-thresh-grid">
        <div class="fr-thresh-block">
          <span class="fr-sec-label">File Size (Custom)</span>
          <div class="fr-compound-input">
            <button type="button" id="fr-size-op" class="fr-op-toggle" title="Click to toggle ≥ / <">${opSym(encodeFilters.sizeOp)}</button>
            <input id="fr-size-val" class="fr-num-input" type="number" min="0" step="any" placeholder="Any" value="${escapeHtml(sizeValStr)}" />
            <button type="button" id="fr-size-unit" class="fr-unit-toggle" title="Click to switch GB / MB">${sizeUnit}</button>
          </div>
          <div class="fr-preset-row">
            ${[[0, "Any"], [500, "500 MB"], [1024, "1 GB"], [2560, "2.5 GB"], [5120, "5 GB"]].map(([v, lbl]) => `
              <button type="button" class="fr-preset-btn ${encodeFilters.minMb === v ? "active" : ""}" data-fr-size-preset="${v}">${lbl}</button>
            `).join("")}
          </div>
        </div>

        <div class="fr-thresh-block">
          <span class="fr-sec-label">Bitrate (Custom)</span>
          <div class="fr-compound-input">
            <button type="button" id="fr-mbps-op" class="fr-op-toggle" title="Click to toggle ≥ / <">${opSym(encodeFilters.bitrateOp)}</button>
            <input id="fr-mbps-val" class="fr-num-input" type="number" min="0" step="any" placeholder="Any" value="${escapeHtml(mbpsValStr)}" />
            <span class="fr-unit-toggle fr-unit-static">Mbps</span>
          </div>
          <div class="fr-preset-row">
            ${[[0, "Any"], [10, "10M"], [20, "20M"], [40, "40M"]].map(([v, lbl]) => `
              <button type="button" class="fr-preset-btn ${encodeFilters.minMbps === v ? "active" : ""}" data-fr-mbps-preset="${v}">${lbl}</button>
            `).join("")}
          </div>
        </div>

        <div class="fr-thresh-block">
          <span class="fr-sec-label">Resolution (Custom)</span>
          <div class="fr-compound-input">
            <button type="button" id="fr-res-op" class="fr-op-toggle" title="Click to toggle ≥ / <">${opSym(encodeFilters.resOp)}</button>
            <input id="fr-res-val" class="fr-num-input" type="number" min="0" step="60" placeholder="Any" value="${escapeHtml(resValStr)}" />
            <span class="fr-unit-toggle fr-unit-static">p</span>
          </div>
          <div class="fr-preset-row">
            ${[[0, "Any"], [720, "720p"], [1080, "1080p"], [2160, "4K"]].map(([v, lbl]) => `
              <button type="button" class="fr-preset-btn ${encodeFilters.resHeight === v ? "active" : ""}" data-fr-res-preset="${v}">${lbl}</button>
            `).join("")}
          </div>
        </div>

        <div class="fr-thresh-block">
          <span class="fr-sec-label">Duration (Custom)</span>
          <div class="fr-compound-input">
            <button type="button" id="fr-dur-op" class="fr-op-toggle" title="Click to toggle ≥ / <">${opSym(encodeFilters.durOp)}</button>
            <input id="fr-dur-val" class="fr-num-input" type="number" min="0" step="any" placeholder="Any" value="${escapeHtml(durValStr)}" />
            <button type="button" id="fr-dur-unit" class="fr-unit-toggle" title="Click to switch min / sec / hr">${durUnit === "h" ? "hr" : durUnit === "s" ? "sec" : "min"}</button>
          </div>
          <div class="fr-preset-row">
            ${[[0, "Any"], [60, "1m"], [300, "5m"], [900, "15m"], [3600, "1h"]].map(([v, lbl]) => `
              <button type="button" class="fr-preset-btn ${encodeFilters.durSecs === v ? "active" : ""}" data-fr-dur-preset="${v}">${lbl}</button>
            `).join("")}
          </div>
        </div>
      </div>

      <div class="fr-divider"></div>

      <div class="fr-footer">
        <div id="fr-match-stat" class="fr-match-stat">
          <span class="fr-match-dot"></span>
          <span><strong>${selectableMatches.length}</strong> match${selectableMatches.length === 1 ? "" : "es"} (${fmtBytes(matchBytes)})</span>
          ${matched.length !== selectableMatches.length ? `<span class="muted">· ${matched.length} total</span>` : ""}
        </div>
        <div class="fr-footer-actions">
          <button type="button" id="fr-act-deselect" class="btn btn-sm btn-secondary" ${selectableMatches.length === 0 ? "disabled" : ""}>− Deselect</button>
          <button type="button" id="fr-act-add" class="btn btn-sm btn-secondary" ${selectableMatches.length === 0 ? "disabled" : ""}>+ Add</button>
          <button type="button" id="fr-act-select" class="btn btn-sm fr-cta-select" ${selectableMatches.length === 0 ? "disabled" : ""}>Select ${selectableMatches.length}</button>
        </div>
      </div>
    `;

    const refreshAll = () => {
      const q = activeQueue();
      const jobs = activeQueueJobs();
      renderQueueList(q, jobs);
      renderActionBar();
    };

    dr.querySelector("#fr-close")?.addEventListener("click", () => {
      dr.classList.add("hidden");
      $("btn-more-filters")?.setAttribute("aria-expanded", "false");
      syncEncodeFilterControls();
    });
    dr.querySelector("#fr-clear-all")?.addEventListener("click", () => {
      resetAllEncodeFilters();
      refreshAll();
    });

    dr.querySelectorAll("[data-fr-smart]").forEach((btn) => {
      btn.onclick = () => {
        const k = btn.getAttribute("data-fr-smart");
        encodeFilters.selectedCodecs.clear();
        if (k === "best_savings") {
          if (bestSavingsActive) {
            encodeFilters.codec = "all";
            encodeFilters.minMb = 0;
          } else {
            encodeFilters.codec = "non_modern";
            encodeFilters.sizeOp = "ge";
            encodeFilters.minMb = 1024;
          }
        } else if (k === "non_modern") {
          encodeFilters.codec = exclModernActive ? "all" : "non_modern";
        } else if (k === "bitrate_20") {
          encodeFilters.minMbps = bitrate20Active ? 0 : 20;
          encodeFilters.bitrateOp = "ge";
        } else if (k === "modern_only") {
          encodeFilters.codec = modernOnlyActive ? "all" : "modern_only";
        }
        refreshAll();
      };
    });

    dr.querySelectorAll("[data-fr-codec-quick]").forEach((btn) => {
      btn.onclick = () => {
        const mode = btn.getAttribute("data-fr-codec-quick");
        encodeFilters.selectedCodecs.clear();
        encodeFilters.codec = mode === "non_modern" ? "non_modern" : "all";
        refreshAll();
      };
    });

    dr.querySelectorAll("[data-fr-codec]").forEach((btn) => {
      btn.onclick = () => {
        const c = btn.getAttribute("data-fr-codec");
        encodeFilters.codec = "all";
        if (encodeFilters.selectedCodecs.has(c)) encodeFilters.selectedCodecs.delete(c);
        else encodeFilters.selectedCodecs.add(c);
        refreshAll();
      };
    });

    // Clickable >= / < toggles (zero dropdowns)
    dr.querySelector("#fr-size-op")?.addEventListener("click", () => {
      encodeFilters.sizeOp = encodeFilters.sizeOp === "lt" ? "ge" : "lt";
      refreshAll();
    });
    dr.querySelector("#fr-mbps-op")?.addEventListener("click", () => {
      encodeFilters.bitrateOp = encodeFilters.bitrateOp === "lt" ? "ge" : "lt";
      refreshAll();
    });
    dr.querySelector("#fr-res-op")?.addEventListener("click", () => {
      encodeFilters.resOp = encodeFilters.resOp === "lt" ? "ge" : "lt";
      refreshAll();
    });
    dr.querySelector("#fr-dur-op")?.addEventListener("click", () => {
      encodeFilters.durOp = encodeFilters.durOp === "lt" ? "ge" : "lt";
      refreshAll();
    });

    // Unit toggles & custom inputs
    dr.querySelector("#fr-size-unit")?.addEventListener("click", () => {
      encodeFilters.sizeUnit = encodeFilters.sizeUnit === "GB" ? "MB" : "GB";
      const val = parseFloat(dr.querySelector("#fr-size-val")?.value || "");
      if (Number.isFinite(val) && val > 0) {
        encodeFilters.minMb = val * (encodeFilters.sizeUnit === "GB" ? 1024 : 1);
      }
      refreshAll();
    });
    dr.querySelector("#fr-size-val")?.addEventListener("input", (e) => {
      const val = parseFloat(e.target.value || "");
      encodeFilters.minMb = Number.isFinite(val) && val > 0 ? val * (encodeFilters.sizeUnit === "GB" ? 1024 : 1) : 0;
      refreshAll();
    });
    dr.querySelectorAll("[data-fr-size-preset]").forEach((btn) => {
      btn.onclick = () => {
        const v = Number(btn.getAttribute("data-fr-size-preset") || 0);
        encodeFilters.minMb = v;
        if (v >= 1024) encodeFilters.sizeUnit = "GB";
        else if (v > 0) encodeFilters.sizeUnit = "MB";
        refreshAll();
      };
    });

    dr.querySelector("#fr-mbps-val")?.addEventListener("input", (e) => {
      const val = parseFloat(e.target.value || "");
      encodeFilters.minMbps = Number.isFinite(val) && val > 0 ? val : 0;
      refreshAll();
    });
    dr.querySelectorAll("[data-fr-mbps-preset]").forEach((btn) => {
      btn.onclick = () => {
        encodeFilters.minMbps = Number(btn.getAttribute("data-fr-mbps-preset") || 0);
        refreshAll();
      };
    });

    dr.querySelector("#fr-res-val")?.addEventListener("input", (e) => {
      const val = parseFloat(e.target.value || "");
      encodeFilters.resHeight = Number.isFinite(val) && val > 0 ? Math.round(val) : 0;
      refreshAll();
    });
    dr.querySelectorAll("[data-fr-res-preset]").forEach((btn) => {
      btn.onclick = () => {
        encodeFilters.resHeight = Number(btn.getAttribute("data-fr-res-preset") || 0);
        refreshAll();
      };
    });

    dr.querySelector("#fr-dur-unit")?.addEventListener("click", () => {
      const order = ["m", "s", "h"];
      encodeFilters.durUnit = order[(order.indexOf(encodeFilters.durUnit) + 1) % order.length];
      const mult = encodeFilters.durUnit === "h" ? 3600 : encodeFilters.durUnit === "s" ? 1 : 60;
      const val = parseFloat(dr.querySelector("#fr-dur-val")?.value || "");
      if (Number.isFinite(val) && val > 0) {
        encodeFilters.durSecs = val * mult;
      }
      refreshAll();
    });
    dr.querySelector("#fr-dur-val")?.addEventListener("input", (e) => {
      const val = parseFloat(e.target.value || "");
      const mult = encodeFilters.durUnit === "h" ? 3600 : encodeFilters.durUnit === "s" ? 1 : 60;
      encodeFilters.durSecs = Number.isFinite(val) && val > 0 ? val * mult : 0;
      refreshAll();
    });
    dr.querySelectorAll("[data-fr-dur-preset]").forEach((btn) => {
      btn.onclick = () => {
        const v = Number(btn.getAttribute("data-fr-dur-preset") || 0);
        encodeFilters.durSecs = v;
        if (v >= 3600) encodeFilters.durUnit = "h";
        else if (v > 0) encodeFilters.durUnit = "m";
        refreshAll();
      };
    });

    // Footer batch selection actions
    dr.querySelector("#fr-act-deselect")?.addEventListener("click", () => {
      selectableMatches.forEach((j) => checkedJobIds.delete(j.id));
      refreshAll();
      renderSidebar();
    });
    dr.querySelector("#fr-act-add")?.addEventListener("click", () => {
      selectableMatches.forEach((j) => checkedJobIds.add(j.id));
      refreshAll();
      renderSidebar();
    });
    dr.querySelector("#fr-act-select")?.addEventListener("click", () => {
      checkedJobIds.clear();
      selectableMatches.forEach((j) => checkedJobIds.add(j.id));
      refreshAll();
      renderSidebar();
    });
  }

  function openColFilterPopover(colKey, anchorEl) {
    closeColFilterPopover();
    const pop = document.createElement("div");
    pop.className = "col-filter-popover";

    const specs = {
      file: { title: "Filter by Name", type: "search" },
      codec: {
        title: "Filter Codec",
        field: "codec",
        options: [
          ["all", "All Codecs"],
          ["non_modern", "Exclude HEVC & AV1"],
          ["only_h264", "Only H.264 / AVC"],
          ["only_legacy", "Only Legacy"],
          ["modern_only", "HEVC / AV1 only"],
        ],
      },
      res: {
        title: "Filter Resolution",
        type: "res_slider",
      },
      duration: {
        title: "Filter Duration",
        type: "numeric_custom",
        opField: "durOp",
        valField: "durSecs",
        options: [
          [0, "Any Duration"],
          [60, "1 min"],
          [300, "5 min"],
          [900, "15 min"],
          [3600, "1 hour"],
        ],
        units: [["s", "sec", 1], ["m", "min", 60], ["h", "hr", 3600]],
        placeholder: "e.g. 12",
      },
      size: {
        title: "Filter File Size",
        type: "numeric_custom",
        opField: "sizeOp",
        valField: "minMb",
        options: [
          [0, "Any Size"],
          [100, "100 MB"],
          [500, "500 MB"],
          [1024, "1 GB"],
          [2048, "2 GB"],
          [5120, "5 GB"],
        ],
        units: [["MB", "MB", 1], ["GB", "GB", 1024]],
        placeholder: "e.g. 250",
      },
      bitrate: {
        title: "Filter Bitrate",
        type: "numeric_custom",
        opField: "bitrateOp",
        valField: "minMbps",
        options: [
          [0, "Any Bitrate"],
          [5, "5 Mbps"],
          [10, "10 Mbps"],
          [20, "20 Mbps"],
          [50, "50 Mbps"],
        ],
        units: [["Mbps", "Mbps", 1]],
        placeholder: "e.g. 8.5",
      },
      target: {
        title: "Filter Target Recipe",
        field: "target",
        options: [
          ["all", "All Recipes"],
          ["hevc", "HEVC"],
          ["av1", "AV1"],
          ["remux", "Remux"],
        ],
      },
      status: {
        title: "Filter Status",
        field: "status",
        options: [
          ["all", "All Statuses"],
          ["listed", "Not Yet Queued"],
          ["selected", "Checked"],
          ["ready", "In Queue"],
          ["completed", "Completed"],
          ["needs_attention", "Needs Attention"],
          ["failed", "Failed / Interrupted"],
        ],
      },
    };
    const cfg = specs[colKey];
    if (!cfg) return;

    if (cfg.type === "search") {
      const sm = compileEncodeSearchMatcher();
      pop.style.minWidth = "250px";
      pop.innerHTML = `
        <div class="col-filter-popover-title">${cfg.title}</div>
        <input id="col-pop-search" class="full-input ${!sm.valid ? "has-error" : ""}" type="search" value="${escapeHtml(encodeFilters.search)}" placeholder="Pattern or substring…" style="height:28px;font-size:12px;" />
        ${!sm.valid ? `<div id="col-pop-regex-err" style="font-size:11px;color:var(--danger,#e5484d);">${escapeHtml(sm.error)}</div>` : `<div id="col-pop-regex-err" class="hidden" style="font-size:11px;color:var(--danger,#e5484d);"></div>`}
        <div class="col-filter-popover-title" style="margin-top:2px;">Match Scope</div>
        <div class="inline-row" style="gap:4px;">
          ${[
            ["name", "File name"],
            ["path", "Full path"],
            ["regex", "Regex"],
          ].map(([m, lbl]) => `<button type="button" class="pill-filter-btn ${encodeFilters.searchMode === m ? "active" : ""}" data-pop-smode="${m}" style="flex:1;justify-content:center;font-size:11px;padding:3px 6px;">${lbl}</button>`).join("")}
        </div>
        <label class="inline-row" style="gap:6px;font-size:11.5px;margin-top:2px;cursor:pointer;">
          <input id="col-pop-sinv" type="checkbox" class="switch-chk" ${encodeFilters.searchInvert ? "checked" : ""} />
          <span>Exclude matching files (invert)</span>
        </label>
      `;
    } else if (cfg.type === "res_slider") {
      pop.style.minWidth = "250px";
      pop.innerHTML = `
        <div class="col-filter-popover-title">${cfg.title}</div>
        <div class="inline-row" style="gap:4px;">
          <button type="button" class="pill-filter-btn ${encodeFilters.resOp === "ge" ? "active" : ""}" data-pop-res-op="ge" style="flex:1;justify-content:center;font-size:11px;padding:4px 6px;">At least (≥)</button>
          <button type="button" class="pill-filter-btn ${encodeFilters.resOp === "lt" ? "active" : ""}" data-pop-res-op="lt" style="flex:1;justify-content:center;font-size:11px;padding:4px 6px;">Smaller than (&lt;)</button>
        </div>
        <div style="margin-top:4px;">
          <div class="setup-row-between" style="font-size:12px;margin-bottom:3px;">
            <span class="muted">Threshold</span>
            <strong id="col-pop-res-readout">${escapeHtml(formatResFilterLabel(encodeFilters.resOp, encodeFilters.resHeight))}</strong>
          </div>
          <input id="col-pop-res-slider" type="range" min="0" max="2160" step="60" value="${encodeFilters.resHeight || 0}" style="width:100%;accent-color:var(--accent-solid);" />
        </div>
        <div class="inline-row" style="gap:4px;flex-wrap:wrap;margin-top:2px;">
          ${[
            [0, "Any"],
            [480, "480p"],
            [720, "720p"],
            [1080, "1080p"],
            [1440, "1440p"],
            [2160, "4K"],
          ].map(([rv, rlbl]) => `<button type="button" class="pill-filter-btn ${encodeFilters.resHeight === rv ? "active" : ""}" data-pop-res-snap="${rv}" style="font-size:11px;padding:2px 7px;">${rlbl}</button>`).join("")}
        </div>
      `;
    } else if (cfg.type === "numeric_custom") {
      const curOp = encodeFilters[cfg.opField] || "ge";
      const curVal = encodeFilters[cfg.valField] || 0;
      const opSign = opSym(curOp);
      pop.style.minWidth = "235px";
      pop.innerHTML = `
        <div class="col-filter-popover-title">${cfg.title}</div>
        <div class="inline-row" style="gap:4px;margin-bottom:2px;">
          <button type="button" class="pill-filter-btn ${curOp === "ge" ? "active" : ""}" data-pop-num-op="ge" style="flex:1;justify-content:center;font-size:11px;padding:3px 6px;">At least (≥)</button>
          <button type="button" class="pill-filter-btn ${curOp === "lt" ? "active" : ""}" data-pop-num-op="lt" style="flex:1;justify-content:center;font-size:11px;padding:3px 6px;">Smaller than (&lt;)</button>
        </div>
        ${cfg.options.map(([val, lbl]) => `
          <button type="button" class="col-filter-opt ${curVal === val ? "active" : ""}" data-pop-val="${val}">
            <span>${val === 0 ? escapeHtml(lbl) : `<span class="pop-op-txt">${opSign}</span> ${escapeHtml(lbl)}`}</span>
            <span>${curVal === val ? "✓" : ""}</span>
          </button>
        `).join("")}
        <div style="border-top:1px solid var(--border);margin-top:4px;padding-top:6px;">
          <div class="col-filter-popover-title" style="margin-bottom:4px;">Custom Threshold (<span class="pop-op-txt">${opSign}</span>)</div>
          <div class="inline-row" style="gap:4px;">
            <input id="col-pop-num-val" class="full-input" type="number" min="0" step="any" placeholder="${escapeHtml(cfg.placeholder || "")}" style="width:74px;height:26px;font-size:12px;" />
            ${cfg.units.length > 1 ? `
              <select id="col-pop-num-unit" class="compact-sel" style="height:26px;">
                ${cfg.units.map(([u, ul]) => `<option value="${u}">${ul}</option>`).join("")}
              </select>
            ` : `<span class="muted" style="font-size:11px;">${cfg.units[0][1]}</span>`}
            <button id="col-pop-num-apply" class="btn btn-sm btn-primary" type="button" style="height:26px;padding:0 8px;">Apply</button>
          </div>
        </div>
      `;
    } else {
      const curVal = encodeFilters[cfg.field];
      pop.innerHTML = `<div class="col-filter-popover-title">${cfg.title}</div>` +
        cfg.options.map(([val, lbl]) => `
          <button type="button" class="col-filter-opt ${curVal === val ? "active" : ""}" data-pop-val="${escapeHtml(String(val))}">
            <span>${escapeHtml(lbl)}</span>
            <span>${curVal === val ? "✓" : ""}</span>
          </button>
        `).join("");
    }

    document.body.appendChild(pop);
    activeColPopover = pop;
    const rect = anchorEl.getBoundingClientRect();
    const left = Math.min(window.innerWidth - pop.offsetWidth - 10, Math.max(8, rect.left - 8));
    pop.style.left = `${left}px`;
    pop.style.top = `${Math.min(window.innerHeight - pop.offsetHeight - 10, rect.bottom + 6)}px`;

    if (cfg.type === "search") {
      const inp = pop.querySelector("#col-pop-search");
      const errBox = pop.querySelector("#col-pop-regex-err");
      inp.focus();
      inp.select();
      const refreshRegexHint = () => {
        const smNow = compileEncodeSearchMatcher();
        inp.classList.toggle("has-error", !smNow.valid);
        if (errBox) {
          errBox.textContent = smNow.error || "";
          errBox.classList.toggle("hidden", smNow.valid);
        }
      };
      inp.addEventListener("input", () => {
        encodeFilters.search = inp.value;
        refreshRegexHint();
        renderQueueList(activeQueue(), activeQueueJobs());
      });
      pop.querySelectorAll("[data-pop-smode]").forEach((mb) => {
        mb.onclick = () => {
          encodeFilters.searchMode = mb.getAttribute("data-pop-smode");
          pop.querySelectorAll("[data-pop-smode]").forEach((x) => x.classList.toggle("active", x === mb));
          refreshRegexHint();
          renderQueueList(activeQueue(), activeQueueJobs());
        };
      });
      pop.querySelector("#col-pop-sinv")?.addEventListener("change", (ev) => {
        encodeFilters.searchInvert = !!ev.target.checked;
        renderQueueList(activeQueue(), activeQueueJobs());
      });
    } else if (cfg.type === "res_slider") {
      const slider = pop.querySelector("#col-pop-res-slider");
      const readout = pop.querySelector("#col-pop-res-readout");
      const syncResPopUI = () => {
        readout.textContent = formatResFilterLabel(encodeFilters.resOp, encodeFilters.resHeight);
        slider.value = String(encodeFilters.resHeight || 0);
        pop.querySelectorAll("[data-pop-res-op]").forEach((b) => b.classList.toggle("active", b.getAttribute("data-pop-res-op") === encodeFilters.resOp));
        pop.querySelectorAll("[data-pop-res-snap]").forEach((b) => b.classList.toggle("active", Number(b.getAttribute("data-pop-res-snap")) === encodeFilters.resHeight));
      };
      pop.querySelectorAll("[data-pop-res-op]").forEach((b) => {
        b.onclick = () => {
          encodeFilters.resOp = b.getAttribute("data-pop-res-op");
          syncResPopUI();
          renderQueueList(activeQueue(), activeQueueJobs());
          renderActionBar();
        };
      });
      slider.addEventListener("input", () => {
        encodeFilters.resHeight = Number(slider.value || 0);
        syncResPopUI();
        renderQueueList(activeQueue(), activeQueueJobs());
        renderActionBar();
      });
      pop.querySelectorAll("[data-pop-res-snap]").forEach((b) => {
        b.onclick = () => {
          encodeFilters.resHeight = Number(b.getAttribute("data-pop-res-snap") || 0);
          syncResPopUI();
          renderQueueList(activeQueue(), activeQueueJobs());
          renderActionBar();
        };
      });
    } else if (cfg.type === "numeric_custom") {
      const nVal = pop.querySelector("#col-pop-num-val");
      const nUnit = pop.querySelector("#col-pop-num-unit");
      const nApply = pop.querySelector("#col-pop-num-apply");
      const curRaw = encodeFilters[cfg.valField] || 0;
      if (curRaw > 0 && nVal) {
        let picked = cfg.units[0];
        for (let i = cfg.units.length - 1; i >= 0; i--) {
          if (curRaw >= cfg.units[i][2]) { picked = cfg.units[i]; break; }
        }
        if (nUnit) nUnit.value = picked[0];
        nVal.value = String(+(curRaw / picked[2]).toFixed(2));
      }
      pop.querySelectorAll("[data-pop-num-op]").forEach((b) => {
        b.onclick = () => {
          const nextOp = b.getAttribute("data-pop-num-op");
          encodeFilters[cfg.opField] = nextOp;
          pop.querySelectorAll("[data-pop-num-op]").forEach((x) => x.classList.toggle("active", x === b));
          pop.querySelectorAll(".pop-op-txt").forEach((s) => { s.textContent = opSym(nextOp); });
          if (encodeFilters[cfg.valField] > 0) {
            renderQueueList(activeQueue(), activeQueueJobs());
            renderActionBar();
          }
        };
      });
      pop.querySelectorAll("[data-pop-val]").forEach((btn) => {
        btn.onclick = () => {
          encodeFilters[cfg.valField] = Number(btn.getAttribute("data-pop-val") || 0);
          closeColFilterPopover();
          renderQueueList(activeQueue(), activeQueueJobs());
          renderActionBar();
        };
      });
      const applyCustomNum = () => {
        const num = parseFloat(nVal?.value || "");
        const mult = nUnit
          ? (cfg.units.find((u) => u[0] === nUnit.value)?.[2] || 1)
          : cfg.units[0][2];
        encodeFilters[cfg.valField] = Number.isFinite(num) && num > 0 ? num * mult : 0;
        closeColFilterPopover();
        renderQueueList(activeQueue(), activeQueueJobs());
        renderActionBar();
      };
      nApply.onclick = applyCustomNum;
      nVal?.addEventListener("keydown", (e) => {
        if (e.key === "Enter") { e.preventDefault(); applyCustomNum(); }
      });
    } else {
      pop.querySelectorAll("[data-pop-val]").forEach((btn) => {
        btn.onclick = () => {
          encodeFilters[cfg.field] = btn.getAttribute("data-pop-val");
          closeColFilterPopover();
          renderQueueList(activeQueue(), activeQueueJobs());
          renderActionBar();
        };
      });
    }
  }

  function renderQueueList(q, allJobs) {
    const searchMatcher = compileEncodeSearchMatcher();
    let jobs = sortEncodeJobs(allJobs.filter((j) => encodeFilterColumnMatch(j, searchMatcher)));

    const totalBytes = allJobs.reduce((a, b) => a + (b.input_bytes || 0), 0);
    const visibleBytes = jobs.reduce((a, b) => a + (b.input_bytes || 0), 0);
    const checkedJobs = allJobs.filter((j) => checkedJobIds.has(j.id) && isEncodeSelectableJob(j));
    const checkedBytes = checkedJobs.reduce((a, b) => a + (b.input_bytes || 0), 0);
    const filterLbl = $("lbl-queue-filter-bar");
    if (filterLbl) {
      const isFiltered = jobs.length !== allJobs.length;
      filterLbl.classList.toggle("hidden", !isFiltered);
      if (isFiltered) {
        filterLbl.textContent = `${jobs.length} of ${allJobs.length} matching (${fmtBytes(visibleBytes)})`;
      }
    }
    if ($("lbl-encode-sel-pill")) {
      $("lbl-encode-sel-pill").textContent = `${checkedJobs.length} selected (${fmtBytes(checkedBytes)})`;
      $("lbl-encode-sel-pill").classList.toggle("hidden", checkedJobs.length === 0);
    }
    if ($("btn-desel-all")) {
      $("btn-desel-all").classList.toggle("hidden", checkedJobs.length === 0);
    }
    syncEncodeFilterControls(searchMatcher);
    renderFilterRulesDrawer(allJobs, jobs);

    const alreadyHevc = allJobs.filter((j) => !isQueuedJob(j) && j.status === "listed" && isModernCodec(jobVideoInfo(j).codec));
    const skipBtn = $("btn-triage-skip-hevc");
    skipBtn.classList.toggle("hidden", alreadyHevc.length === 0);
    if (alreadyHevc.length > 0) skipBtn.textContent = `Remove ${alreadyHevc.length} HEVC/AV1`;

    const hasEncoding = allJobs.some((j) => j.status === "encoding");
    $("btn-live-monitor").classList.toggle("hidden", !hasEncoding);

    const hasRetryable = allJobs.some((j) => ["failed", "canceled", "interrupted", "needs_attention"].includes(j.status) && j.status_reason !== "Not selected");
    const hasFinished = allJobs.some((j) => ["completed", "below_savings_target", "canceled"].includes(j.status));
    $("btn-retry-failed").classList.toggle("hidden", !hasRetryable);
    $("btn-clear-completed").classList.toggle("hidden", !hasFinished);

    const rowsEl = $("queue-rows");
    rowsEl.innerHTML = "";
    encodeRowNodes = new Map();
    renderedEncodeJobs = jobs;
    renderedEncodeAllJobs = allJobs;
    encodeJobsById = new Map(jobs.map((job) => [job.id, job]));
    const activeProg = snapshot.active_progress;

    const selectableVisible = jobs.filter(isEncodeSelectableJob);
    const checkedVisibleCount = selectableVisible.filter((j) => checkedJobIds.has(j.id)).length;
    const allVisibleChecked = selectableVisible.length > 0 && checkedVisibleCount === selectableVisible.length;
    const someVisibleChecked = checkedVisibleCount > 0 && !allVisibleChecked;

    const funnelSvg = `<svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><polygon points="22 3 2 3 10 12.46 10 19 14 21 14 12.46 22 3"/></svg>`;
    const colHdr = (key, label, rightAlign, filtered) => {
      const sorted = encodeSort.col === key;
      const sortGlyph = sorted ? (encodeSort.dir === "asc" ? "↑" : "↓") : "⇅";
      const sortTitle = sorted
        ? `Sorted ${encodeSort.dir === "asc" ? "ascending" : "descending"} (click to switch / reset)`
        : `Click to sort by ${label}`;
      return `
        <th class="col-th ${rightAlign ? "col-th-right" : ""}" style="${rightAlign ? "text-align:right;" : ""}">
          <span class="col-th-inner">
            <button type="button" class="col-sort-btn ${sorted ? "is-sorted" : ""}" data-col-sort="${key}" title="${escapeHtml(sortTitle)}">
              <span>${label}</span>
              <span class="col-sort-icon" aria-hidden="true">${sortGlyph}</span>
            </button>
            <button type="button" class="col-flt-btn ${filtered ? "is-filtered" : ""}" data-col-flt="${key}" title="Filter ${label}">
              ${funnelSvg}
            </button>
          </span>
        </th>
      `;
    };

    const tbl = document.createElement("table");
    tbl.className = "transcode-table";
    tbl.innerHTML = `
      <thead>
        <tr>
          <th style="width:34px;"><input type="checkbox" id="tc-chk-master" ${allVisibleChecked ? "checked" : ""} ${selectableVisible.length === 0 ? "disabled" : ""} title="Toggle all visible selectable files" /></th>
          ${colHdr("file", "File", false, Boolean(encodeFilters.search.trim()))}
          ${colHdr("codec", "Codec", false, encodeFilters.codec !== "all")}
          ${colHdr("res", "Resolution", false, encodeFilters.resHeight > 0)}
          ${colHdr("duration", "Duration", false, encodeFilters.durSecs > 0)}
          ${colHdr("size", "Size", true, encodeFilters.minMb > 0)}
          ${colHdr("bitrate", "Bitrate", true, encodeFilters.minMbps > 0)}
          ${colHdr("target", "Target Recipe", false, encodeFilters.target !== "all")}
          ${colHdr("status", "Status", false, encodeFilters.status !== "all")}
          <th style="text-align:right;">Actions</th>
        </tr>
      </thead>
      <tbody></tbody>
    `;
    const tbody = tbl.querySelector("tbody");
    const masterChk = tbl.querySelector("#tc-chk-master");
    if (masterChk) masterChk.indeterminate = someVisibleChecked;

    masterChk.addEventListener("change", (e) => {
      if (e.target.checked) selectableVisible.forEach((j) => checkedJobIds.add(j.id));
      else selectableVisible.forEach((j) => checkedJobIds.delete(j.id));
      syncEncodeSelectionUi();
      renderActionBar();
      renderSidebar();
    });

    tbody.addEventListener("click", (e) => {
      const row = e.target.closest("tr.tc-row");
      if (!row) return;
      const id = row.dataset.jobId;
      const job = encodeJobsById.get(id);
      if (!job) return;
      if (e.target.closest(".job-chevron")) {
        if (expandedJobIds.has(id)) expandedJobIds.delete(id);
        else expandedJobIds.add(id);
        renderQueueList(q, allJobs);
        return;
      }
      if (e.target.closest("button, input")) return;
      encodeRowNodes.get(selectedJobId)?.classList.remove("selected");
      selectedJobId = id;
      row.classList.add("selected");
      sidebarMode = "job";
      sidebarOpen = true;
      renderSidebar();
    });
    tbody.addEventListener("change", (e) => {
      if (!e.target.matches(".job-chk")) return;
      const id = e.target.closest("tr.tc-row")?.dataset.jobId;
      if (!id) return;
      if (e.target.checked) checkedJobIds.add(id);
      else checkedJobIds.delete(id);
      syncEncodeSelectionUi();
      renderActionBar();
      renderSidebar();
    });

    tbl.querySelectorAll("[data-col-sort]").forEach((btn) => {
      btn.addEventListener("click", () => {
        const col = btn.getAttribute("data-col-sort");
        const numFirst = ["res", "duration", "size", "bitrate"].includes(col);
        if (encodeSort.col !== col) {
          encodeSort = { col, dir: numFirst ? "desc" : "asc" };
        } else if (encodeSort.dir === (numFirst ? "desc" : "asc")) {
          encodeSort = { col, dir: numFirst ? "asc" : "desc" };
        } else {
          encodeSort = { col: null, dir: "desc" };
        }
        renderQueueList(q, allJobs);
      });
    });

    tbl.querySelectorAll("[data-col-flt]").forEach((btn) => {
      btn.addEventListener("click", (e) => {
        e.stopPropagation();
        const col = btn.getAttribute("data-col-flt");
        if (activeColPopover && activeColPopover.dataset?.col === col) {
          closeColFilterPopover();
          return;
        }
        openColFilterPopover(col, btn);
        if (activeColPopover) activeColPopover.dataset.col = col;
      });
    });

    jobs.forEach((j) => {
      const isSel = j.id === selectedJobId;
      const isExp = expandedJobIds.has(j.id);
      const isChecked = checkedJobIds.has(j.id);
      const canChk = isEncodeSelectableJob(j);
      const prog = activeProg && activeProg.job_id === j.id ? activeProg : null;
      const shared = sharedQueueName(j);
      const info = jobVideoInfo(j);

      const targetDesc = j.recipe.family === "remux"
        ? `Remux · ${j.recipe.container.toUpperCase()}`
        : `${j.recipe.codec.toUpperCase()} CRF ${j.recipe.crf} · ${j.recipe.container.toUpperCase()}`;

      let statusHtml = "";
      if (j.status === "completed" && j.output_bytes) {
        statusHtml = `<span class="badge badge-ok">Done ${fmtRatio(j.input_bytes, j.output_bytes)}</span>`;
      } else if (prog) {
        statusHtml = buildInlineProgressHtml(j, prog, false);
      } else if (["verifying", "publishing"].includes(j.status)) {
        statusHtml = `<span class="badge badge-active">${escapeHtml(statusLabel(j.status))}</span>`;
      } else if (info.isPendingStats) {
        statusHtml = `<span class="badge">Computing stats…</span>`;
      } else if (["ready", "queued"].includes(j.status)) {
        statusHtml = `<span class="badge badge-active">In Queue</span>`;
      } else if (j.status === "listed" || (j.status === "canceled" && j.status_reason === "Not selected")) {
        statusHtml = isChecked ? `<span class="badge badge-active">Selected</span>` : `<span class="badge">Ready</span>`;
      } else {
        statusHtml = `<span class="badge ${statusBadgeClass(j.status)}">${escapeHtml(statusLabel(j.status))}</span>`;
      }

      const codecBadgeClass = isModernCodec(info.codec) ? "badge-warn" : "";

      const tr = document.createElement("tr");
      tr.dataset.jobId = j.id;
      tr.className = "tc-row" + (isSel ? " selected" : "") + (canChk && !isChecked ? " tc-unchecked" : "");
      tr.innerHTML = `
        <td><input type="checkbox" class="job-chk" ${isChecked ? "checked" : ""} ${canChk ? "" : "disabled"} /></td>
        <td class="min-w-0" style="max-width:290px;">
          <div class="inline-row" style="gap:6px;">
            <button type="button" class="job-chevron ${isExp ? "open" : ""}" aria-expanded="${isExp}" title="Toggle stream tracks">
              <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><polyline points="9 18 15 12 9 6"/></svg>
            </button>
            ${jobStatusIconHtml(j.status)}
            <div class="min-w-0" style="flex:1;">
              <div class="inline-row" style="gap:5px;">
                <span class="job-name truncate" title="${escapeHtml(j.source_path)}">${escapeHtml(jobFileName(j))}</span>
                ${shared ? `<span class="badge badge-warn">${escapeHtml(shared)}</span>` : ""}
              </div>
              <div class="mono muted truncate" style="font-size:10.5px;">${escapeHtml(j.source_rel_path)}</div>
            </div>
          </div>
        </td>
        <td><span class="badge ${codecBadgeClass}">${escapeHtml(info.codec)}</span></td>
        <td class="tabular-nums muted">${escapeHtml(info.res)}</td>
        <td class="tabular-nums muted">${info.dur > 0 ? fmtDuration(info.dur) : "—"}</td>
        <td class="tabular-nums" style="text-align:right;font-weight:600;">${fmtBytes(j.input_bytes)}</td>
        <td class="tabular-nums muted" style="text-align:right;">${info.mbps > 0 ? `${info.mbps} Mbps` : "—"}</td>
        <td class="muted">${escapeHtml(targetDesc)}</td>
        <td data-status-cell>${statusHtml}</td>
        <td style="text-align:right;"><div class="row-actions-slot inline-row" style="justify-content:flex-end;gap:3px;"></div></td>
      `;

      const acts = tr.querySelector(".row-actions-slot");
      if (j.status === "encoding") {
        const b = document.createElement("button");
        b.type = "button";
        b.className = "btn btn-sm btn-secondary";
        b.textContent = "Live";
        b.onclick = (e) => { e.stopPropagation(); openLivePreviewModal(j.id); };
        acts.appendChild(b);
      }
      if (j.status === "completed") {
        const b = document.createElement("button");
        b.type = "button";
        b.className = "btn btn-sm btn-secondary";
        b.textContent = "Show";
        b.onclick = (e) => { e.stopPropagation(); invoke("reveal_output", { path: j.destination_path }); };
        acts.appendChild(b);
      }
      if (["failed", "interrupted", "needs_attention"].includes(j.status) || (j.status === "canceled" && j.status_reason !== "Not selected")) {
        const b = document.createElement("button");
        b.type = "button";
        b.className = "btn btn-sm btn-secondary";
        b.textContent = "Retry";
        b.onclick = (e) => { e.stopPropagation(); invoke("retry_jobs", { jobIds: [j.id] }); };
        acts.appendChild(b);
      }
      if (canChk || ["ready", "queued", "needs_attention", "interrupted", "failed", "completed", "below_savings_target"].includes(j.status)) {
        const rm = document.createElement("button");
        rm.type = "button";
        rm.className = "btn btn-ghost btn-sm";
        rm.textContent = "×";
        rm.title = isQueuedJob(j)
          ? "Remove from Queue (returns video to list)"
          : "Remove from tab (keeps file on disk)";
        rm.onclick = (e) => {
          e.stopPropagation();
          checkedJobIds.delete(j.id);
          if (isQueuedJob(j)) invoke("dequeue_jobs", { jobIds: [j.id] });
          else invoke("remove_pending_jobs", { jobIds: [j.id] });
        };
        acts.appendChild(rm);
      }

      tbody.appendChild(tr);
      encodeRowNodes.set(j.id, tr);

      if (j.status === "needs_attention" && j.error_message) {
        const errTr = document.createElement("tr");
        errTr.className = "tc-subrow";
        const canMkv = j.plan?.issues?.some((iss) => iss.can_use_mkv);
        errTr.innerHTML = `
          <td colspan="10">
            <div class="inline-row" style="justify-content:space-between;gap:8px;flex-wrap:wrap;">
              <span style="font-size:11.5px;color:var(--warn);"><strong>${escapeHtml(j.error_message)}</strong></span>
              <div class="inline-row" style="gap:6px;">
                <button type="button" class="btn btn-sm btn-secondary" data-fwd-rep="${j.id}">Send to Repair</button>
                ${canMkv ? `<button type="button" class="btn btn-sm btn-primary" data-fix-mkv="${j.id}">Use MKV</button>` : ""}
              </div>
            </div>
          </td>
        `;
        errTr.querySelector("[data-fwd-rep]").onclick = (e) => {
          e.stopPropagation();
          openForwardModal([j.id], "repair", "file needing repair", false);
        };
        errTr.querySelector("[data-fix-mkv]")?.addEventListener("click", (e) => {
          e.stopPropagation();
          invoke("update_pending_job", { jobId: j.id, patch: { recipe: { ...j.recipe, container: "mkv" } } });
        });
        tbody.appendChild(errTr);
      }

      if (isExp) {
        const detTr = document.createElement("tr");
        detTr.className = "tc-subrow";
        detTr.innerHTML = `<td colspan="10">${renderExpandedMediaHtml(j)}</td>`;
        tbody.appendChild(detTr);
      }
    });

    rowsEl.appendChild(tbl);
  }

  function syncEncodeSelectionUi() {
    let checkedCount = 0;
    let checkedBytes = 0;
    let selectableVisible = 0;
    let checkedVisible = 0;
    for (const job of renderedEncodeAllJobs) {
      if (checkedJobIds.has(job.id) && isEncodeSelectableJob(job)) {
        checkedCount++;
        checkedBytes += job.input_bytes || 0;
      }
    }
    for (const job of renderedEncodeJobs) {
      if (!isEncodeSelectableJob(job)) continue;
      selectableVisible++;
      const checked = checkedJobIds.has(job.id);
      if (checked) checkedVisible++;
      const row = encodeRowNodes.get(job.id);
      if (!row) continue;
      row.classList.toggle("tc-unchecked", !checked);
      const box = row.querySelector(".job-chk");
      if (box) box.checked = checked;
      const status = row.querySelector("[data-status-cell]");
      if (status && (job.status === "listed" || (job.status === "canceled" && job.status_reason === "Not selected"))) {
        status.innerHTML = checked ? '<span class="badge badge-active">Selected</span>' : '<span class="badge">Ready</span>';
      }
    }
    const pill = $("lbl-encode-sel-pill");
    if (pill) {
      pill.textContent = `${checkedCount} selected (${fmtBytes(checkedBytes)})`;
      pill.classList.toggle("hidden", checkedCount === 0);
    }
    $("btn-desel-all")?.classList.toggle("hidden", checkedCount === 0);
    const master = $("tc-chk-master");
    if (master) {
      master.checked = selectableVisible > 0 && checkedVisible === selectableVisible;
      master.indeterminate = checkedVisible > 0 && checkedVisible < selectableVisible;
    }
  }

  function buildInlineProgressHtml(job, prog, detailed = true) {
    const dur = prog.duration_secs || 1;
    const pct = Math.min(100, (prog.out_time_secs / dur) * 100);
    const outSz = prog.bytes_written > 0 ? fmtBytes(prog.bytes_written) : "0 B";
    const ratio = prog.est_final_ratio && prog.est_final_ratio > 0 ? prog.est_final_ratio : null;
    const savedPct = ratio ? Math.round((1 - 1 / ratio) * 100) : null;
    const ratioStr = ratio ? `${ratio.toFixed(2)}:1 (${savedPct >= 0 ? `-${savedPct}%` : `+${Math.abs(savedPct)}%`})` : null;
    const spStr = prog.speed ? `${prog.speed.toFixed(2)}x` : null;
    const topParts = [`${pct.toFixed(1)}%`];
    if (spStr) topParts.push(spStr);
    topParts.push(outSz);
    if (ratioStr) topParts.push(ratioStr);
    topParts.push(`ETA ${prog.eta_secs ? fmtDuration(prog.eta_secs) : "—"}`);
    return `<div class="tabular-nums" style="min-width:${detailed ? 210 : 185}px;"><div style="font-size:11px;font-weight:600;color:var(--accent-text);">${topParts.join(" · ")}</div><div class="progress-track" style="height:5px;margin-top:3px;"><div class="progress-fill" style="width:${pct.toFixed(1)}%"></div></div></div>`;
  }

  function buildQueueCompressionCellHtml(job, prog) {
    if (job.status === "completed" && job.output_bytes) {
      const ratio = job.input_bytes > 0 ? job.input_bytes / job.output_bytes : null;
      const savedPct = ratio && ratio > 0 ? Math.round((1 - 1 / ratio) * 100) : null;
      return `
        <div class="tabular-nums">
          <div style="font-weight:600;color:var(--ok);">${escapeHtml(fmtRatio(job.input_bytes, job.output_bytes))}${savedPct !== null ? ` <span class="muted" style="font-weight:500;">(${savedPct >= 0 ? `-${savedPct}%` : `+${Math.abs(savedPct)}%`})</span>` : ""}</div>
          <div class="mono muted" style="font-size:10.5px;">Out ${fmtBytes(job.output_bytes)}</div>
        </div>
      `;
    }
    if (prog && prog.job_id === job.id) {
      const outSz = prog.bytes_written > 0 ? fmtBytes(prog.bytes_written) : "0 B";
      const ratio = prog.est_final_ratio && prog.est_final_ratio > 0 ? prog.est_final_ratio : null;
      const savedPct = ratio ? Math.round((1 - 1 / ratio) * 100) : null;
      const estFinal = ratio && job.input_bytes > 0 ? Math.round(job.input_bytes / ratio) : null;
      const etaStr = prog.eta_secs ? fmtDuration(prog.eta_secs) : "—";
      return `
        <div class="tabular-nums">
          <div style="font-weight:600;color:var(--accent-text);">${ratio ? `${ratio.toFixed(2)}:1 (${savedPct >= 0 ? `-${savedPct}%` : `+${Math.abs(savedPct)}%`})` : "Estimating…"}</div>
          <div class="mono muted" style="font-size:10.5px;">Now ${outSz}${estFinal ? ` → ~${fmtBytes(estFinal)}` : ""} · ETA ${etaStr}</div>
        </div>
      `;
    }
    if (["encoding", "verifying", "publishing"].includes(job.status)) {
      return `<span class="muted" style="font-size:11px;">Starting…</span>`;
    }
    return `<span class="muted">—</span>`;
  }

  function renderSpeedChartSvg(history) {
    const pts = (history || []).filter((p) => p && typeof p.speed === "number" && p.speed > 0);
    if (pts.length < 2) {
      return `<div class="muted" style="font-size:11px;padding:18px 0;text-align:center;">Collecting live speed samples… diagram renders once 2+ progress ticks arrive.</div>`;
    }
    const minT = pts[0].elapsed_secs;
    const maxT = Math.max(minT + 1, pts[pts.length - 1].elapsed_secs);
    const rawMaxSp = Math.max(...pts.map((p) => p.speed));
    const maxSp = Math.max(0.25, rawMaxSp * 1.15);

    const xPct = (t) => ((t - minT) / (maxT - minT)) * 100;
    const yPct = (sp) => 100 - (Math.min(sp, maxSp) / maxSp) * 100;

    const polyCoords = pts.map((p) => `${xPct(p.elapsed_secs).toFixed(2)},${yPct(p.speed).toFixed(2)}`);
    const linePath = `M ${polyCoords.join(" L ")}`;
    const areaPath = `${linePath} L ${xPct(pts[pts.length - 1].elapsed_secs).toFixed(2)},100 L ${xPct(pts[0].elapsed_secs).toFixed(2)},100 Z`;

    const last = pts[pts.length - 1];
    const peak = pts.reduce((a, b) => (b.speed > a.speed ? b : a), pts[0]);

    const ref1y = yPct(1.0);
    const ref1Line = maxSp >= 1.0
      ? `<line x1="0" y1="${ref1y.toFixed(2)}" x2="100" y2="${ref1y.toFixed(2)}" stroke="currentColor" stroke-opacity="0.32" stroke-width="1" stroke-dasharray="4 3" vector-effect="non-scaling-stroke" />`
      : "";
    const ref1Tag = maxSp >= 1.0
      ? `<span class="qp-ref-tag" style="top:${ref1y.toFixed(2)}%;">1.0x realtime</span>`
      : "";

    const midT = (minT + maxT) * 0.5;
    const fmtTick = (v) => `${v.toFixed(v >= 2 ? 1 : 2)}x`;

    return `
      <div class="qp-chart-layout">
        <div class="qp-y-axis">
          <span>${fmtTick(maxSp)}</span>
          <span>${fmtTick(maxSp * 0.5)}</span>
          <span>0.00x</span>
        </div>
        <div class="qp-plot-col">
          <div class="qp-plot-area">
            <svg class="qp-speed-svg" viewBox="0 0 100 100" preserveAspectRatio="none" style="color:var(--text);">
              <defs>
                <linearGradient id="qpSpeedGrad" x1="0" y1="0" x2="0" y2="1">
                  <stop offset="0%" stop-color="var(--accent, #3b82f6)" stop-opacity="0.30" />
                  <stop offset="100%" stop-color="var(--accent, #3b82f6)" stop-opacity="0.02" />
                </linearGradient>
              </defs>
              <line x1="0" y1="0.5" x2="100" y2="0.5" stroke="currentColor" stroke-opacity="0.12" stroke-width="1" stroke-dasharray="3 3" vector-effect="non-scaling-stroke" />
              <line x1="0" y1="50" x2="100" y2="50" stroke="currentColor" stroke-opacity="0.12" stroke-width="1" stroke-dasharray="3 3" vector-effect="non-scaling-stroke" />
              <line x1="0" y1="99.5" x2="100" y2="99.5" stroke="currentColor" stroke-opacity="0.14" stroke-width="1" vector-effect="non-scaling-stroke" />
              ${ref1Line}
              <path d="${areaPath}" fill="url(#qpSpeedGrad)" />
              <path d="${linePath}" fill="none" stroke="var(--accent-text, var(--accent, #3b82f6))" stroke-width="2" stroke-linejoin="round" stroke-linecap="round" vector-effect="non-scaling-stroke" />
            </svg>
            ${ref1Tag}
            <span class="qp-dot-marker qp-dot-peak" style="left:${xPct(peak.elapsed_secs).toFixed(2)}%;top:${yPct(peak.speed).toFixed(2)}%;" title="Peak ${peak.speed.toFixed(2)}x"></span>
            <span class="qp-dot-marker qp-dot-cur" style="left:${xPct(last.elapsed_secs).toFixed(2)}%;top:${yPct(last.speed).toFixed(2)}%;" title="Current ${last.speed.toFixed(2)}x"></span>
          </div>
          <div class="qp-x-axis">
            <span>${fmtDuration(minT)}</span>
            <span>${fmtDuration(midT)}</span>
            <span>${fmtDuration(maxT)}</span>
          </div>
        </div>
      </div>
    `;
  }

  function buildQueueTelemetryHtml(job, prog, history) {
    if (!job) return "";
    const p = prog && prog.job_id === job.id ? prog : null;
    const dur = p?.duration_secs || jobVideoInfo(job).dur || 0;
    const outTime = p?.out_time_secs || 0;
    const pct = dur > 0 ? Math.min(100, (outTime / dur) * 100) : 0;
    const bytesOut = p?.bytes_written || 0;
    const ratio = p?.est_final_ratio || null;
    const estFinalBytes = ratio && ratio > 0 ? Math.round(job.input_bytes / ratio) : null;
    const savedPct = ratio && ratio > 0 ? Math.round((1 - 1 / ratio) * 100) : null;
    const outMbps = outTime > 1 && bytesOut > 0 ? ((bytesOut * 8) / outTime / 1_000_000).toFixed(2) : null;

    const pts = (history || []).filter((x) => typeof x.speed === "number" && x.speed > 0);
    const curSpeed = p?.speed ?? (pts.length ? pts[pts.length - 1].speed : null);
    const avgSpeed = pts.length ? pts.reduce((s, x) => s + x.speed, 0) / pts.length : curSpeed;
    const peakSpeed = pts.length ? Math.max(...pts.map((x) => x.speed)) : curSpeed;

    const ratioMain = ratio ? `${ratio.toFixed(2)}:1` : "Estimating…";
    const ratioSub = ratio
      ? `${savedPct}% smaller · Est. ${fmtBytes(estFinalBytes)}${dur > 0 && outTime / dur < 0.10 ? " (early)" : ""}`
      : (outTime > 0 ? "Waiting for ≥2s output data" : `Source: ${fmtBytes(job.input_bytes)}`);

    const outMain = bytesOut > 0 ? fmtBytes(bytesOut) : "0 B";
    const outSub = `of ${fmtBytes(job.input_bytes)}${outMbps ? ` · ~${outMbps} Mbps out` : ""}`;

    const speedMain = curSpeed ? `${curSpeed.toFixed(2)}x` : "—";
    const speedSubParts = [];
    if (p?.fps) speedSubParts.push(`${p.fps.toFixed(1)} fps`);
    if (avgSpeed) speedSubParts.push(`Avg ${avgSpeed.toFixed(2)}x`);
    if (peakSpeed) speedSubParts.push(`Peak ${peakSpeed.toFixed(2)}x`);

    const progMain = `${pct.toFixed(1)}%`;
    const progSub = `${fmtDuration(outTime)} / ${dur > 0 ? fmtDuration(dur) : "—"} · ETA ${p?.eta_secs ? fmtDuration(p.eta_secs) : "—"}`;

    return `
      <div class="qp-telemetry-card">
        <div class="qp-tel-head">
          <div class="inline-row" style="gap:8px;min-width:0;flex-wrap:wrap;">
            <span class="badge badge-active">${escapeHtml(statusLabel(job.status))}</span>
            <strong class="truncate" style="font-size:13px;" title="${escapeHtml(job.source_path)}">${escapeHtml(jobFileName(job))}</strong>
            <span class="muted tabular-nums" style="font-size:11.5px;">Elapsed ${p ? fmtDuration(p.elapsed_secs) : "—"}</span>
          </div>
          <div class="progress-track" style="width:200px;height:6px;max-width:100%;">
            <div class="progress-fill" style="width:${pct.toFixed(1)}%"></div>
          </div>
        </div>

        <div class="qp-tel-grid">
          <div class="qp-tel-kpi">
            <div class="qp-tel-label">Est. Compression Ratio</div>
            <div class="qp-tel-value">${ratioMain}</div>
            <div class="qp-tel-sub" title="${escapeHtml(ratioSub)}">${escapeHtml(ratioSub)}</div>
          </div>
          <div class="qp-tel-kpi">
            <div class="qp-tel-label">Current Output Size</div>
            <div class="qp-tel-value">${outMain}</div>
            <div class="qp-tel-sub" title="${escapeHtml(outSub)}">${escapeHtml(outSub)}</div>
          </div>
          <div class="qp-tel-kpi">
            <div class="qp-tel-label">Encode Speed</div>
            <div class="qp-tel-value">${speedMain}</div>
            <div class="qp-tel-sub">${speedSubParts.length ? speedSubParts.join(" · ") : "Measuring speed…"}</div>
          </div>
          <div class="qp-tel-kpi">
            <div class="qp-tel-label">Progress &amp; ETA</div>
            <div class="qp-tel-value">${progMain}</div>
            <div class="qp-tel-sub">${progSub}</div>
          </div>
        </div>

        <div class="qp-chart-box">
          <div class="qp-chart-head">
            <strong>Encode Speed Over Time (realtime multiplier)</strong>
            <span class="muted tabular-nums">● Current ${curSpeed ? curSpeed.toFixed(2) + "x" : "—"} · Peak ${peakSpeed ? peakSpeed.toFixed(2) + "x" : "—"} · Avg ${avgSpeed ? avgSpeed.toFixed(2) + "x" : "—"}</span>
          </div>
          ${renderSpeedChartSvg(pts)}
        </div>
      </div>
    `;
  }

  function updateQueueTelemetryDom(prog) {
    const wrap = $("qp-telemetry-wrap");
    if (!wrap || !prog?.job_id) return;
    const job = queueJobsById.get(prog.job_id) || (snapshot?.jobs || []).find((x) => x.id === prog.job_id);
    if (!job || !["encoding", "verifying", "publishing"].includes(job.status)) return;
    wrap.innerHTML = buildQueueTelemetryHtml(job, prog, liveSpeedHistory);
  }

  function updateEncodeProgressDom(progress) {
    if (!progress?.job_id) return;
    const row = encodeRowNodes.get(progress.job_id);
    if (!row) return;
    const job = encodeJobsById.get(progress.job_id);
    if (!job || job.status !== "encoding") return;
    const cell = row.querySelector("[data-status-cell]");
    if (!cell) return;
    cell.innerHTML = buildInlineProgressHtml(job, progress, false);
  }

  function updateQueuePageProgressDom(progress) {
    updateQueueTelemetryDom(progress);
    if (!progress?.job_id) return;
    const row = queuePageNodes.get(progress.job_id);
    if (!row) return;
    const job = queueJobsById.get(progress.job_id);
    if (!job || job.status !== "encoding") return;
    const cell = row.querySelector("[data-q-status-cell]");
    if (cell) cell.innerHTML = buildInlineProgressHtml(job, progress, true);
    const ratioCell = row.querySelector("[data-q-ratio-cell]");
    if (ratioCell) ratioCell.innerHTML = buildQueueCompressionCellHtml(job, progress);
  }

  function openEnqueueConsentModal(q, resp, jobIds) {
    openModal(
      "Confirm Batch Source Cleanup",
      `<p><strong>Automatic Trash is enabled for "${escapeHtml(q.name)}".</strong> Move ${resp.consent_jobs.length} verified originals (${fmtBytes(resp.consent_total_bytes)}) to OS Trash after full decode verification?</p>`,
      [
        {
          label: "Cancel",
          cls: "btn btn-ghost",
          onClick: closeModal,
        },
        {
          label: "Keep originals (recommended)",
          cls: "btn btn-primary",
          onClick: async () => {
            await invoke("update_queue_settings", {
              queueId: q.id,
              patch: { ...q, cleanup_policy: "keep_originals" },
            });
            closeModal();
            await invoke("enqueue_jobs", { queueId: q.id, jobIds, startNow: true, consentConfirmed: false });
            jobIds.forEach((id) => checkedJobIds.delete(id));
            showToast(`Added ${jobIds.length} video${jobIds.length === 1 ? "" : "s"} to the Encode Queue — processing started.`, "success");
          },
        },
        {
          label: `Trash ${resp.consent_jobs.length} verified originals`,
          cls: "btn btn-danger",
          onClick: async () => {
            closeModal();
            await invoke("enqueue_jobs", { queueId: q.id, jobIds, startNow: true, consentConfirmed: true });
            jobIds.forEach((id) => checkedJobIds.delete(id));
            showToast(`Added ${jobIds.length} video${jobIds.length === 1 ? "" : "s"} to the Encode Queue — processing started.`, "success");
          },
        },
      ]
    );
  }

  function openResumeQueueConsentModal(resp) {
    openModal(
      "Confirm Automatic Cleanup to Resume Queue",
      `<p><strong>Trash after verification is enabled</strong> on ${resp.consent_jobs.length} queued file(s) (${fmtBytes(resp.consent_total_bytes)}).</p>`,
      [
        {
          label: "Cancel",
          cls: "btn btn-ghost",
          onClick: closeModal,
        },
        {
          label: "Confirm & Resume Queue",
          cls: "btn btn-danger",
          onClick: async () => {
            closeModal();
            await invoke("start_all_ready_queues", { consentConfirmed: true });
          },
        },
      ]
    );
  }

  function renderTabQueueView(q, tabJobs) {
    const el = $("tab-encode-queue-view");
    if (!el || !q || !snapshot) return;

    const allQueued = (tabJobs || [])
      .filter(isQueuedJob)
      .sort((a, b) => (a.position || 0) - (b.position || 0));

    const activeList = allQueued.filter((j) =>
      ["ready", "queued", "probing", "encoding", "verifying", "publishing"].includes(j.status)
    );
    const completedList = allQueued.filter((j) => j.status === "completed");
    const failedList = allQueued.filter((j) =>
      ["failed", "interrupted", "below_savings_target", "needs_attention", "canceled"].includes(j.status)
    );

    const visible = queuePageFilter === "active"
      ? activeList
      : queuePageFilter === "completed"
        ? completedList
        : queuePageFilter === "failed"
          ? failedList
          : allQueued;

    const removableVisible = visible.filter((j) =>
      !["probing", "encoding", "verifying", "publishing"].includes(j.status)
    );
    const checkedRemovable = allQueued.filter((j) =>
      queueCheckedIds.has(j.id) && !["probing", "encoding", "verifying", "publishing"].includes(j.status)
    );
    const allChecked = removableVisible.length > 0 && removableVisible.every((j) => queueCheckedIds.has(j.id));
    const someChecked = removableVisible.some((j) => queueCheckedIds.has(j.id)) && !allChecked;

    const isEncodingNow = allQueued.find((j) => ["encoding", "verifying", "publishing"].includes(j.status));
    const hasPausedWaiting = Boolean(q.is_paused) && allQueued.some((j) =>
      ["ready", "queued", "interrupted"].includes(j.status)
    );
    const retryableIds = allQueued.filter((j) => ["failed", "interrupted", "canceled", "needs_attention"].includes(j.status)).map((j) => j.id);
    const finishedIds = allQueued.filter((j) => ["completed", "below_savings_target", "canceled"].includes(j.status)).map((j) => j.id);
    const activeProg = snapshot.active_progress && snapshot.active_progress.queue_id === q.id
      ? snapshot.active_progress
      : null;

    el.innerHTML = `
      <div class="queue-page-header">
        <div>
          <div class="inline-row" style="gap:10px;flex-wrap:wrap;">
            <h2 style="margin:0;font-size:16px;">${escapeHtml(q.name)} · Queue</h2>
            <span class="counts-pill tabular-nums">${activeList.length} active/waiting · ${completedList.length} completed${failedList.length ? ` · ${failedList.length} failed` : ""}</span>
          </div>
          <div class="muted" style="font-size:12px;margin-top:3px;">Showing queued &amp; active jobs for this tab. Removing an item takes it off the Queue while keeping it in this tab's Video List.</div>
        </div>
        <div class="inline-row" style="gap:6px;flex-wrap:wrap;">
          <button type="button" id="qp-back-videos" class="btn btn-sm btn-secondary">← Back to Video List</button>
          ${isEncodingNow ? `<button type="button" id="qp-live" class="btn btn-sm btn-secondary"><span class="dot pulse"></span> Live Preview</button>` : ""}
          ${isEncodingNow ? `<button type="button" id="qp-pause" class="btn btn-sm btn-warn-outline">Pause After File</button>` : ""}
          ${isEncodingNow ? `<button type="button" id="qp-cancel" class="btn btn-sm btn-danger-outline">Cancel Current</button>` : ""}
          ${(!isEncodingNow && hasPausedWaiting) ? `<button type="button" id="qp-resume" class="btn btn-sm btn-primary">Resume Queue</button>` : ""}
          ${retryableIds.length > 0 ? `<button type="button" id="qp-retry-all" class="btn btn-sm btn-secondary">Retry Failed (${retryableIds.length})</button>` : ""}
          ${checkedRemovable.length > 0 ? `<button type="button" id="qp-remove-checked" class="btn btn-sm btn-danger-outline">Remove ${checkedRemovable.length} from Queue</button>` : ""}
          ${finishedIds.length > 0 ? `<button type="button" id="qp-clear-finished" class="btn btn-sm btn-ghost">Clear Finished (${finishedIds.length})</button>` : ""}
        </div>
      </div>

      <div class="queue-page-subbar">
        <div class="inline-row" style="gap:6px;flex-wrap:wrap;">
          <button type="button" class="pill-filter-btn ${queuePageFilter === "all" ? "active" : ""}" data-qp-flt="all">All (${allQueued.length})</button>
          <button type="button" class="pill-filter-btn ${queuePageFilter === "active" ? "active" : ""}" data-qp-flt="active">Queued &amp; Active (${activeList.length})</button>
          <button type="button" class="pill-filter-btn ${queuePageFilter === "completed" ? "active" : ""}" data-qp-flt="completed">Completed (${completedList.length})</button>
          <button type="button" class="pill-filter-btn ${queuePageFilter === "failed" ? "active" : ""}" data-qp-flt="failed">Failed / Interrupted (${failedList.length})</button>
        </div>
      </div>

      ${isEncodingNow ? `<div id="qp-telemetry-wrap">${buildQueueTelemetryHtml(isEncodingNow, activeProg, liveSpeedHistory)}</div>` : `<div id="qp-telemetry-wrap" class="hidden"></div>`}

      ${visible.length === 0 ? `
        <div class="queue-empty-box">
          <strong>${allQueued.length === 0 ? "This tab's Queue is empty" : "No queued items match this filter"}</strong>
          <p class="muted" style="font-size:12px;margin:6px 0 12px;">${allQueued.length === 0 ? "Select videos in this tab's Video List and click \"Add to Queue\" — they enter here and begin processing right away." : "Switch filter pills above to inspect other queued items."}</p>
          ${allQueued.length === 0 ? `<button type="button" id="qp-empty-back" class="btn btn-sm btn-primary">Browse Videos in This Tab</button>` : ""}
        </div>
      ` : `<div id="qp-table-wrap" class="transcode-table-wrap"></div>`}
    `;

    el.querySelector("#qp-back-videos")?.addEventListener("click", () => {
      setActiveEncodeSubView(q, "list");
      renderWorkspace();
      renderActionBar();
    });
    el.querySelector("#qp-empty-back")?.addEventListener("click", () => {
      setActiveEncodeSubView(q, "list");
      renderWorkspace();
      renderActionBar();
    });
    el.querySelectorAll("[data-qp-flt]").forEach((b) => {
      b.onclick = () => { queuePageFilter = b.getAttribute("data-qp-flt"); renderTabQueueView(q, tabJobs); };
    });
    el.querySelector("#qp-resume")?.addEventListener("click", async () => {
      try {
        const resp = await invoke("start_queue", { queueId: q.id, consentConfirmed: null });
        if (resp && resp.requires_cleanup_consent) openResumeQueueConsentModal(resp);
      } catch (err) {
        showToast(String(err), "warn");
      }
    });
    el.querySelector("#qp-pause")?.addEventListener("click", () => {
      if (isEncodingNow) invoke("pause_after_current", { queueId: isEncodingNow.queue_id });
    });
    el.querySelector("#qp-cancel")?.addEventListener("click", () => invoke("cancel_current_file", { jobId: null }));
    el.querySelector("#qp-live")?.addEventListener("click", () => {
      if (isEncodingNow) openLivePreviewModal(isEncodingNow.id);
    });
    el.querySelector("#qp-retry-all")?.addEventListener("click", () => {
      if (retryableIds.length > 0) invoke("retry_jobs", { jobIds: retryableIds });
    });
    el.querySelector("#qp-remove-checked")?.addEventListener("click", async () => {
      const ids = checkedRemovable.map((j) => j.id);
      if (!ids.length) return;
      await invoke("dequeue_jobs", { jobIds: ids });
      ids.forEach((id) => queueCheckedIds.delete(id));
      showToast(`Removed ${ids.length} item(s) from Queue (still in Video List).`, "info");
    });
    el.querySelector("#qp-clear-finished")?.addEventListener("click", async () => {
      if (!finishedIds.length) return;
      await invoke("dequeue_jobs", { jobIds: finishedIds });
      finishedIds.forEach((id) => queueCheckedIds.delete(id));
      showToast(`Cleared ${finishedIds.length} finished item(s) from Queue.`, "info");
    });

    const wrap = el.querySelector("#qp-table-wrap");
    if (!wrap) return;

    const tbl = document.createElement("table");
    tbl.className = "transcode-table";
    tbl.innerHTML = `
      <thead>
        <tr>
          <th style="width:34px;"><input type="checkbox" id="qp-chk-master" ${allChecked ? "checked" : ""} ${removableVisible.length === 0 ? "disabled" : ""} /></th>
          <th style="width:76px;">#</th>
          <th>Video &amp; Tab</th>
          <th>Target Recipe</th>
          <th style="text-align:right;">Source</th>
          <th>Compression &amp; Output</th>
          <th>Queue Status</th>
          <th style="text-align:right;">Actions</th>
        </tr>
      </thead>
      <tbody></tbody>
    `;
    const tbody = tbl.querySelector("tbody");
    const masterChk = tbl.querySelector("#qp-chk-master");
    if (masterChk) masterChk.indeterminate = someChecked;
    masterChk?.addEventListener("change", (e) => {
      if (e.target.checked) removableVisible.forEach((j) => queueCheckedIds.add(j.id));
      else removableVisible.forEach((j) => queueCheckedIds.delete(j.id));
      renderTabQueueView(q, tabJobs);
    });

    queuePageNodes = new Map();
    queueJobsById = new Map(visible.map((j) => [j.id, j]));

    visible.forEach((j, idx) => {
      const qObj = q;
      const info = jobVideoInfo(j);
      const isBusy = ["probing", "encoding", "verifying", "publishing"].includes(j.status);
      const prog = activeProg && activeProg.job_id === j.id ? activeProg : null;
      const targetDesc = j.recipe.family === "remux"
        ? `Remux · ${j.recipe.container.toUpperCase()}`
        : `${j.recipe.codec.toUpperCase()} CRF ${j.recipe.crf} · ${j.recipe.container.toUpperCase()}`;

      let stHtml = "";
      if (j.status === "completed" && j.output_bytes) {
        stHtml = `<span class="badge badge-ok">Done ${fmtRatio(j.input_bytes, j.output_bytes)} · ${fmtBytes(j.output_bytes)}</span>`;
      } else if (prog) {
        stHtml = buildInlineProgressHtml(j, prog, true);
      } else if (["encoding", "verifying", "publishing", "probing"].includes(j.status)) {
        stHtml = `<span class="badge badge-active">${escapeHtml(statusLabel(j.status))}</span>`;
      } else if (["ready", "queued"].includes(j.status)) {
        stHtml = qObj?.is_paused
          ? `<span class="badge">Paused</span>`
          : `<span class="badge badge-active">Queued</span>`;
      } else {
        stHtml = `<span class="badge ${statusBadgeClass(j.status)}">${escapeHtml(statusLabel(j.status))}</span>`;
      }

      const sameQWaiting = allQueued.filter((x) => x.queue_id === j.queue_id && ["ready", "queued"].includes(x.status));
      const waitPos = sameQWaiting.findIndex((x) => x.id === j.id);
      const canUp = waitPos > 0;
      const canDn = waitPos >= 0 && waitPos < sameQWaiting.length - 1;

      const tr = document.createElement("tr");
      tr.className = "tc-row";
      tr.dataset.jobId = j.id;
      tr.innerHTML = `
        <td><input type="checkbox" class="qp-row-chk" ${queueCheckedIds.has(j.id) ? "checked" : ""} ${isBusy ? "disabled" : ""} /></td>
        <td class="tabular-nums">
          <div class="inline-row" style="gap:3px;">
            <span class="muted" style="min-width:18px;">${idx + 1}</span>
            ${canUp ? `<button type="button" class="btn btn-ghost btn-sm qp-mv-up" title="Move earlier" style="padding:0 4px;">↑</button>` : ""}
            ${canDn ? `<button type="button" class="btn btn-ghost btn-sm qp-mv-dn" title="Move later" style="padding:0 4px;">↓</button>` : ""}
          </div>
        </td>
        <td class="min-w-0" style="max-width:300px;">
          <div class="inline-row" style="gap:6px;">
            ${jobStatusIconHtml(j.status)}
            <div class="min-w-0" style="flex:1;">
              <div class="inline-row" style="gap:6px;flex-wrap:wrap;">
                <strong class="truncate" title="${escapeHtml(j.source_path)}">${escapeHtml(jobFileName(j))}</strong>
                ${qObj ? `<span class="badge">${escapeHtml(qObj.name)}</span>` : ""}
              </div>
              <div class="mono muted truncate" style="font-size:10.5px;">${escapeHtml(info.codec)} · ${escapeHtml(info.res)} · ${info.dur > 0 ? fmtDuration(info.dur) : "—"}</div>
            </div>
          </div>
        </td>
        <td class="min-w-0" style="max-width:220px;">
          <div>${escapeHtml(targetDesc)}</div>
          <div class="mono muted truncate" style="font-size:10.5px;" title="${escapeHtml(j.destination_path)}">→ ${escapeHtml(j.destination_path.split(/[/\\]/).pop() || "")}</div>
        </td>
        <td class="tabular-nums" style="text-align:right;font-weight:600;">${fmtBytes(j.input_bytes)}</td>
        <td data-q-ratio-cell>${buildQueueCompressionCellHtml(j, prog)}</td>
        <td data-q-status-cell>${stHtml}</td>
        <td style="text-align:right;">
          <div class="inline-row" style="justify-content:flex-end;gap:4px;">
            ${j.status === "encoding" ? `<button type="button" class="btn btn-sm btn-secondary qp-act-live">Live</button>` : ""}
            ${j.status === "completed" ? `<button type="button" class="btn btn-sm btn-secondary qp-act-show">Show</button>` : ""}
            ${["failed", "interrupted", "canceled", "needs_attention"].includes(j.status) ? `<button type="button" class="btn btn-sm btn-secondary qp-act-retry">Retry</button>` : ""}
            ${!isBusy ? `<button type="button" class="btn btn-sm btn-ghost qp-act-deq" title="Remove from Queue (keeps file in Video List)">Remove</button>` : ""}
          </div>
        </td>
      `;

      tr.querySelector(".qp-row-chk")?.addEventListener("change", (ev) => {
        if (ev.target.checked) queueCheckedIds.add(j.id);
        else queueCheckedIds.delete(j.id);
        renderTabQueueView(q, tabJobs);
      });
      tr.querySelector(".qp-mv-up")?.addEventListener("click", async () => {
        const ids = (snapshot.jobs || []).filter((x) => x.queue_id === j.queue_id).map((x) => x.id);
        const a = ids.indexOf(j.id);
        const b = ids.indexOf(sameQWaiting[waitPos - 1].id);
        if (a >= 0 && b >= 0) {
          [ids[a], ids[b]] = [ids[b], ids[a]];
          await invoke("reorder_jobs", { queueId: j.queue_id, orderedJobIds: ids });
        }
      });
      tr.querySelector(".qp-mv-dn")?.addEventListener("click", async () => {
        const ids = (snapshot.jobs || []).filter((x) => x.queue_id === j.queue_id).map((x) => x.id);
        const a = ids.indexOf(j.id);
        const b = ids.indexOf(sameQWaiting[waitPos + 1].id);
        if (a >= 0 && b >= 0) {
          [ids[a], ids[b]] = [ids[b], ids[a]];
          await invoke("reorder_jobs", { queueId: j.queue_id, orderedJobIds: ids });
        }
      });
      tr.querySelector(".qp-act-live")?.addEventListener("click", () => openLivePreviewModal(j.id));
      tr.querySelector(".qp-act-show")?.addEventListener("click", () => invoke("reveal_output", { path: j.destination_path }));
      tr.querySelector(".qp-act-retry")?.addEventListener("click", () => invoke("retry_jobs", { jobIds: [j.id] }));
      tr.querySelector(".qp-act-deq")?.addEventListener("click", () => {
        queueCheckedIds.delete(j.id);
        invoke("dequeue_jobs", { jobIds: [j.id] });
      });

      tbody.appendChild(tr);
      queuePageNodes.set(j.id, tr);

      if (["failed", "needs_attention"].includes(j.status) && j.error_message) {
        const errRow = document.createElement("tr");
        errRow.className = "tc-subrow";
        errRow.innerHTML = `<td colspan="8"><span style="font-size:11.5px;color:var(--warn);"><strong>${escapeHtml(j.error_message)}</strong></span></td>`;
        tbody.appendChild(errRow);
      }
    });

    wrap.appendChild(tbl);
  }

  function renderExpandedMediaHtml(j) {
    if (!j.probe) return `<p class="muted">No ffprobe stream details cached yet.</p>`;
    const f = j.probe.format;
    const summary = `Container: <strong>${escapeHtml(f.format_long_name || f.format_name || "—")}</strong> · Size: <strong>${fmtBytes(j.input_bytes)}</strong> · Duration: <strong>${fmtDuration(f.duration_secs)}</strong> · Bitrate: <strong>${f.bit_rate ? Math.round(f.bit_rate / 1000) + " kbps" : "—"}</strong>`;
    let rows = "";
    for (const s of j.probe.streams) {
      const specs = s.codec_type === "video"
        ? `${s.width || "?"}×${s.height || "?"} · ${s.r_frame_rate || s.avg_frame_rate || "—"} fps · ${s.pix_fmt || "—"}`
        : s.codec_type === "audio"
          ? `${s.channel_layout || (s.channels ? s.channels + " ch" : "—")} · ${s.sample_rate ? s.sample_rate + " Hz" : "—"}`
          : (s.title || "—");
      rows += `<tr><td>#${s.index}</td><td>${escapeHtml(s.codec_type)}</td><td>${escapeHtml(s.codec_name)}</td><td>${escapeHtml(s.language || "—")}</td><td>${escapeHtml(specs)}</td></tr>`;
    }
    return `<div>${summary}</div><table class="media-tracks-table"><thead><tr><th>#</th><th>Type</th><th>Codec</th><th>Lang</th><th>Specs</th></tr></thead><tbody>${rows}</tbody></table>`;
  }

  function imageMimeFromPath(p) {
    const ext = (p || "").split(".").pop()?.toLowerCase() || "";
    if (ext === "png") return "image/png";
    if (ext === "webp") return "image/webp";
    if (ext === "gif") return "image/gif";
    if (ext === "bmp") return "image/bmp";
    return "image/jpeg";
  }

  async function loadLocalImageUrl(mediaPath) {
    const bytes = await invoke("read_local_media_preview", { mediaPath });
    return URL.createObjectURL(new Blob([new Uint8Array(bytes)], { type: imageMimeFromPath(mediaPath) }));
  }

  async function openMediaPreviewModal(item) {
    const path = item.source_path || item.path || "";
    const name = item.name || (item.source_rel_path || path).split(/[\\/]/).pop() || "Media";
    const isImg = isImagePath(path);
    const dur = Math.max(1, Math.floor(item.duration_secs || item.probe?.format?.duration_secs || 60));
    let seekT = Math.min(Math.floor(dur * 0.25), Math.max(0, dur - 1));
    let activeBlobUrl = null;

    const wrap = document.createElement("div");
    wrap.className = "sidebar-panel-stack";
    wrap.innerHTML = `
      <div class="inline-row" style="justify-content:space-between;font-size:12px;flex-wrap:wrap;gap:6px;">
        <span class="mono muted truncate" style="max-width:460px;">${escapeHtml(path)}</span>
        <span class="badge">${isImg ? "Image" : "Video"}${item.input_bytes ? " · " + fmtBytes(item.input_bytes) : ""}</span>
      </div>
      <div class="media-preview-stage" style="min-height:240px;">
        <span id="mprev-status" class="muted" style="font-size:12px;">Loading preview…</span>
        <img id="mprev-img" class="hidden" alt="${escapeHtml(name)}" />
      </div>
      ${isImg ? "" : `
        <div class="inline-row" style="gap:8px;flex-wrap:wrap;">
          <input type="range" id="mprev-seek" min="0" max="${dur}" step="1" value="${seekT}" style="flex:1;min-width:160px;" />
          <span id="mprev-time" class="mono muted tabular-nums" style="font-size:11px;min-width:52px;">${fmtDuration(seekT)}</span>
          <button type="button" class="btn btn-sm btn-secondary" data-jump="0.1">10%</button>
          <button type="button" class="btn btn-sm btn-secondary" data-jump="0.5">50%</button>
          <button type="button" class="btn btn-sm btn-secondary" data-jump="0.8">80%</button>
        </div>
      `}
    `;

    const imgEl = wrap.querySelector("#mprev-img");
    const statusEl = wrap.querySelector("#mprev-status");

    const loadFrame = async (tSec) => {
      statusEl.textContent = "Loading preview…";
      statusEl.classList.remove("hidden");
      try {
        let nextUrl;
        if (isImg) {
          nextUrl = await loadLocalImageUrl(path);
        } else {
          const bytes = await invoke("get_comparison_frame", {
            mediaPath: path,
            timestampSecs: tSec,
            fullResolution: false,
          });
          nextUrl = URL.createObjectURL(new Blob([new Uint8Array(bytes)], { type: "image/jpeg" }));
        }
        if (activeBlobUrl) URL.revokeObjectURL(activeBlobUrl);
        activeBlobUrl = nextUrl;
        imgEl.src = nextUrl;
        imgEl.classList.remove("hidden");
        statusEl.classList.add("hidden");
      } catch (err) {
        statusEl.textContent = `Preview unavailable: ${String(err)}`;
        imgEl.classList.add("hidden");
      }
    };

    if (!isImg) {
      const seekInp = wrap.querySelector("#mprev-seek");
      const timeLbl = wrap.querySelector("#mprev-time");
      seekInp.oninput = (e) => {
        seekT = Number(e.target.value);
        timeLbl.textContent = fmtDuration(seekT);
      };
      seekInp.onchange = () => loadFrame(seekT);
      wrap.querySelectorAll("[data-jump]").forEach((b) => {
        b.onclick = () => {
          seekT = Math.max(0, Math.floor(dur * Number(b.getAttribute("data-jump"))));
          seekInp.value = String(seekT);
          timeLbl.textContent = fmtDuration(seekT);
          loadFrame(seekT);
        };
      });
    }

    const buttons = [];
    if (!isImg && path) {
      buttons.push({
        label: "Open in Default Player",
        cls: "btn btn-secondary",
        onClick: () => invoke("open_output_player", { path }),
      });
    }
    buttons.push({
      label: "Close",
      cls: "btn btn-primary",
      onClick: () => {
        if (activeBlobUrl) URL.revokeObjectURL(activeBlobUrl);
        closeModal();
      },
    });

    openModalEl(`Preview · ${name}`, wrap, buttons);
    loadFrame(seekT);
  }

  function renderJobTagsHtml(jobId, tags) {
    const list = tags || [];
    return `
      <div class="inline-row" style="gap:4px;flex-wrap:wrap;margin-top:4px;">
        ${list.map((t) => `<span class="tag-chip">${escapeHtml(t)}<button type="button" class="tag-chip-x" data-untag="${escapeHtml(jobId)}::${escapeHtml(t)}" title="Remove tag">×</button></span>`).join("")}
        <button type="button" class="btn-link" style="font-size:11px;" data-add-tag-job="${escapeHtml(jobId)}">+ Tag</button>
      </div>
    `;
  }

  function renderOrganizeView(q, jobs) {
    const el = $("mode-view-organize");
    const sideRail = $("org-side-rail");
    const tabPane = $("org-tab-pane");
    const embeddedHost = $("org-embedded-tools");
    if (!el || !sideRail || !tabPane || !embeddedHost) return;

    if (!organizeOverview || organizeOverviewQueueId !== q.id) {
      loadOrganizeOverview(q.id, false);
    }

    const ov = organizeOverview && organizeOverviewQueueId === q.id
      ? organizeOverview
      : { job_tags: {}, available_tags: [], ocr_detections: [], face_groups: [], hash_cache: snapshot.hash_cache_stats || {} };
    const cacheStats = ov.hash_cache || snapshot.hash_cache_stats || { entry_count: 0, total_hashed_bytes: 0, cache_bytes_estimate: 0 };
    const groups = snapshot.tab_dedupe_groups?.[q.id] || [];
    const jobById = new Map(jobs.map((j) => [j.id, j]));

    const dupItemCount = groups.reduce((acc, g) => acc + g.items.length, 0);
    const tabSideItems = [
      { id: "largest", label: "Largest Files", count: jobs.length },
      { id: "duplicates", label: "Duplicates & Similar", count: dupItemCount || groups.length },
      { id: "watermarks", label: "Watermarks & Tags", count: (ov.ocr_detections || []).length },
      { id: "faces", label: "People & Faces", count: (ov.face_groups || []).length },
      { id: "folder", label: "Sort to Folder", count: (ov.available_tags || []).length },
    ];
    const toolSideItems = [
      { id: "library", label: "Catalog Library", count: null },
      { id: "catalog_dups", label: "Catalog Duplicates", count: null },
      { id: "batch", label: "Batch Rename & AI", count: null },
      ...(catalogFacesEnabled ? [{ id: "catalog_people", label: "Catalog People", count: null }] : []),
      { id: "integrity", label: "Integrity & PAR2", count: null },
      { id: "tasks", label: "Tasks & Budgets", count: latestTasksCount > 0 ? latestTasksCount : null },
    ];

    const renderSideBtn = (s) => `
      <button type="button" class="org-side-tab ${organizeSideTab === s.id ? "active" : ""}" data-org-side="${s.id}">
        <span>${escapeHtml(s.label)}</span>
        ${s.count !== null && s.count !== undefined ? `<span class="org-side-count">${s.count}</span>` : ""}
      </button>
    `;
    sideRail.innerHTML = `
      <div class="org-side-group-label">This Tab</div>
      ${tabSideItems.map(renderSideBtn).join("")}
      <div class="org-side-group-label" style="margin-top:6px;">Library &amp; Tools</div>
      ${toolSideItems.map(renderSideBtn).join("")}
    `;

    sideRail.querySelectorAll("[data-org-side]").forEach((btn) => {
      btn.onclick = () => {
        organizeSideTab = btn.getAttribute("data-org-side");
        movePreviewResult = null;
        renderOrganizeView(q, jobs);
        renderActionBar();
        renderSidebar();
      };
    });

    const embeddedPanelId = EMBEDDED_ORG_PANELS[organizeSideTab];
    if (embeddedPanelId) {
      tabPane.classList.add("hidden");
      tabPane.innerHTML = "";
      embeddedHost.classList.remove("hidden");
      Object.values(EMBEDDED_ORG_PANELS).forEach((pid) => {
        const panel = $(pid);
        if (panel) panel.classList.toggle("hidden", pid !== embeddedPanelId);
      });
      window.dispatchEvent(new CustomEvent("ms-org-tool-activated", { detail: { tool: organizeSideTab } }));
      return;
    }

    embeddedHost.classList.add("hidden");
    tabPane.classList.remove("hidden");

    if (jobs.length === 0) {
      tabPane.innerHTML = `
        <div class="queue-empty-box">
          <strong>No files attached to "${escapeHtml(q.name)}" yet</strong>
          <p class="muted" style="font-size:12px;margin:6px 0 12px;">Attach videos or a folder to analyze largest files, duplicates, watermarks, faces, and tag sorting in this tab — or browse global tools on the left.</p>
          <div class="inline-row" style="justify-content:center;gap:8px;">
            <button type="button" id="org-empty-folder" class="btn btn-sm btn-primary">Browse Folder…</button>
            <button type="button" id="org-empty-files" class="btn btn-sm btn-secondary">Add Files…</button>
          </div>
        </div>
      `;
      tabPane.querySelector("#org-empty-folder")?.addEventListener("click", () => $("btn-add-folder")?.click());
      tabPane.querySelector("#org-empty-files")?.addEventListener("click", () => $("btn-add-files")?.click());
      return;
    }

    const orgSel = getOrgSelectedSet(q.id);
    let mainHtml = "";

    if (organizeSideTab === "largest") {
      const sorted = [...jobs].sort((a, b) => (b.input_bytes || 0) - (a.input_bytes || 0));
      const maxBytes = sorted[0]?.input_bytes || 1;
      const totalBytes = sorted.reduce((a, b) => a + (b.input_bytes || 0), 0);
      const selectedTrashBytes = sorted.filter((j) => orgSel.has(j.id)).reduce((a, b) => a + (b.input_bytes || 0), 0);

      mainHtml = `
        <div class="summary-hero-card">
          <div>
            <strong style="font-size:14px;">Largest Files in Collection · ${fmtBytes(totalBytes)} Total</strong>
            <div class="muted" style="font-size:12px;margin-top:3px;">Sorted largest first across all ${jobs.length} videos &amp; images in this tab. Preview, apply tags, forward big videos to compress, or move unwanted files to Trash.</div>
          </div>
          <div class="inline-row" style="gap:8px;flex-wrap:wrap;">
            ${orgSel.size > 0 ? `<button type="button" id="org-trash-selected" class="btn btn-sm btn-danger">Move ${orgSel.size} (${fmtBytes(selectedTrashBytes)}) to Trash</button>` : ""}
            <button type="button" id="org-fwd-largest" class="btn btn-sm btn-primary" ${jobs.length === 0 ? "disabled" : ""}>Forward Videos → Transcode</button>
          </div>
        </div>
        <div class="sidebar-panel-stack">
          ${sorted.map((j, idx) => {
            const name = jobFileName(j);
            const info = jobVideoInfo(j);
            const isImg = isImagePath(j.source_path);
            const prot = isProtectedSource(j.source_path);
            const tags = ov.job_tags?.[j.id] || [];
            const pct = Math.max(4, Math.round(((j.input_bytes || 0) / maxBytes) * 100));
            return `
              <div class="job-card ${selectedJobId === j.id ? "selected" : ""}" data-org-job-card="${escapeHtml(j.id)}">
                <div class="job-top-row" style="align-items:flex-start;">
                  <div class="inline-row min-w-0" style="gap:10px;flex:1;align-items:flex-start;">
                    <input type="checkbox" class="org-del-chk" data-org-del-id="${escapeHtml(j.id)}" ${orgSel.has(j.id) ? "checked" : ""} ${prot ? "disabled" : ""} style="margin-top:4px;" />
                    <div class="min-w-0" style="flex:1;">
                      <div class="inline-row" style="gap:6px;flex-wrap:wrap;">
                        <span class="muted tabular-nums" style="font-size:11px;font-weight:700;">#${idx + 1}</span>
                        <strong style="font-size:13px;" class="truncate">${escapeHtml(name)}</strong>
                        <span class="badge">${isImg ? "Image" : escapeHtml(info.res)}</span>
                        ${prot ? `<span class="badge badge-warn">Protected ★</span>` : ""}
                      </div>
                      <div class="mono muted truncate" style="font-size:11px;margin-top:2px;">${escapeHtml(j.source_path)}</div>
                      <div style="height:4px;border-radius:999px;background:var(--surface-2);overflow:hidden;margin-top:6px;max-width:340px;">
                        <div style="height:100%;width:${pct}%;background:var(--accent);"></div>
                      </div>
                      ${renderJobTagsHtml(j.id, tags)}
                    </div>
                  </div>
                  <div class="inline-row tabular-nums" style="gap:8px;flex-shrink:0;">
                    <div style="text-align:right;">
                      <div style="font-size:13px;font-weight:700;">${fmtBytes(j.input_bytes)}</div>
                      <div class="muted" style="font-size:10.5px;">${isImg ? "Photo / Image" : `${info.dur} · ${info.mbps} Mbps`}</div>
                    </div>
                    <button type="button" class="btn btn-sm btn-secondary" data-preview-job="${escapeHtml(j.id)}">Preview</button>
                  </div>
                </div>
              </div>
            `;
          }).join("")}
        </div>
      `;
    } else if (organizeSideTab === "duplicates") {
      const redundant = groups
        .filter((g) => String(g.match_type || "").startsWith("Exact"))
        .flatMap((g) => g.items)
        .filter((i) => i.role === "trash" && !i.trashed);
      const selectedCount = orgSel.size;
      const selectedBytes = Array.from(orgSel).reduce((acc, jid) => acc + (jobById.get(jid)?.input_bytes || 0), 0);

      mainHtml = `
        <div class="org-cache-bar">
          <div class="inline-row" style="gap:8px;flex-wrap:wrap;">
            <strong>SQLite Hash Cache:</strong>
            <span class="tabular-nums">${cacheStats.entry_count || 0} files cached · ${fmtBytes(cacheStats.total_hashed_bytes || 0)} indexed · ${fmtBytes(cacheStats.cache_bytes_estimate || 0)} cache DB</span>
          </div>
          <div class="inline-row" style="gap:8px;">
            <button type="button" id="org-clear-cache" class="btn btn-sm btn-secondary" ${(cacheStats.entry_count || 0) === 0 ? "disabled" : ""}>Delete Hash Cache</button>
          </div>
        </div>
        <div class="summary-hero-card">
          <div>
            <strong style="font-size:14px;">${groups.length} Exact / Similar Group${groups.length === 1 ? "" : "s"}</strong>
            <div class="muted" style="font-size:12px;margin-top:3px;">Uses cached BLAKE3/SHA-256 + perceptual frame/image hashes in SQLite. Exact copies can be bulk-selected safely; visually similar clips require manual review before Trashing.</div>
          </div>
          <div class="inline-row" style="gap:8px;flex-wrap:wrap;">
            <button type="button" id="btn-tab-dedupe-scan" class="btn btn-sm btn-secondary" ${dedupeScanning || jobs.length < 2 ? "disabled" : ""}>${dedupeScanning ? "Scanning…" : "Scan Duplicates & Similar"}</button>
            ${redundant.length > 0 ? `<button type="button" id="org-select-redundant" class="btn btn-sm btn-secondary">Select Recommended (${redundant.length})</button>` : ""}
            ${selectedCount > 0 ? `<button type="button" id="org-trash-selected" class="btn btn-sm btn-danger">Review Trash (${selectedCount} · ${fmtBytes(selectedBytes)})</button>` : ""}
          </div>
        </div>
        ${groups.length === 0 ? `
          <div class="setup-card" style="padding:24px;text-align:center;">
            <strong style="font-size:13px;">${jobs.length < 2 ? "Attach at least 2 videos or images to find duplicates" : "No exact or similar duplicates computed yet (or every file is unique)"}</strong>
            <p class="muted" style="font-size:12px;margin:4px 0 0;">Click <strong>Scan Duplicates &amp; Similar</strong> above. File hashes are stored in SQLite so future scans skip re-hashing unchanged files.</p>
          </div>
        ` : `<div class="sidebar-panel-stack">
          ${groups.map((g) => {
            const isExact = String(g.match_type || "").startsWith("Exact");
            return `
            <section class="dedupe-group-box">
              <div class="dedupe-group-head">
                <strong>${escapeHtml(g.match_type)}</strong>
                <span class="muted tabular-nums">${isExact ? `${g.items.length} copies` : `${g.items.length} clips · Visual Review (check audio &amp; ending)`}</span>
              </div>
              <div>
                ${g.items.map((it) => {
                  const j = jobById.get(it.job_id);
                  const prot = j ? isProtectedSource(j.source_path) : false;
                  const checked = orgSel.has(it.job_id);
                  const roleLabel = it.trashed ? "In OS Trash" : it.role === "keep" ? "Master Copy" : isExact ? "Exact Duplicate" : "Visual Review";
                  const roleCls = it.trashed ? "" : it.role === "keep" ? "badge-ok" : isExact ? "badge-warn" : "";
                  return `
                    <div class="dedupe-member-row ${it.trashed ? "trashed" : ""}">
                      <div class="inline-row min-w-0" style="gap:10px;flex:1;">
                        <input type="checkbox" class="org-del-chk" data-org-del-id="${escapeHtml(it.job_id)}" ${checked ? "checked" : ""} ${it.trashed || prot ? "disabled" : ""} />
                        <div class="min-w-0" style="flex:1;">
                          <div class="inline-row" style="gap:6px;flex-wrap:wrap;">
                            <strong style="font-size:12.5px;">${escapeHtml(it.name)}</strong>
                            <span class="badge ${roleCls}">${roleLabel}</span>
                            ${prot ? `<span class="badge badge-warn">Protected ★</span>` : ""}
                          </div>
                          <div class="mono muted truncate" style="font-size:11px;margin-top:2px;">${escapeHtml(it.rel_path)}</div>
                        </div>
                      </div>
                      <div class="inline-row tabular-nums" style="gap:8px;">
                        <div style="text-align:right;">
                          <div style="font-size:12px;font-weight:600;">${fmtBytes(it.input_bytes)}</div>
                          <div class="muted" style="font-size:10.5px;">${escapeHtml(it.resolution)} · ${it.bitrate_mbps} Mbps</div>
                        </div>
                        ${j ? `<button type="button" class="btn btn-sm btn-secondary" data-preview-job="${escapeHtml(j.id)}">Preview</button>` : ""}
                      </div>
                    </div>
                  `;
                }).join("")}
              </div>
            </section>
          `;
          }).join("")}
        </div>`}
      `;
    } else if (organizeSideTab === "watermarks") {
      const hits = ov.ocr_detections || [];
      const jobByPath = new Map();
      for (const j of jobs) jobByPath.set(j.source_path, j);

      mainHtml = `
        <div class="summary-hero-card">
          <div>
            <strong style="font-size:14px;">Watermark Extraction &amp; Tagging</strong>
            <div class="muted" style="font-size:12px;margin-top:3px;">Scans corner &amp; frame regions on videos and images to extract channel handles, watermarks, and studio overlays into searchable SQLite tags.</div>
          </div>
          <div class="inline-row" style="gap:8px;">
            <button type="button" id="org-ocr-scan" class="btn btn-sm btn-primary" ${ocrScanning || jobs.length === 0 ? "disabled" : ""}>${ocrScanning ? "Extracting Watermarks…" : "Extract Watermarks"}</button>
          </div>
        </div>
        ${hits.length > 0 ? `
          <div class="setup-card">
            <span class="setup-kicker">Detected Watermarks &amp; Text (${hits.length})</span>
            <div class="sidebar-panel-stack" style="gap:8px;">
              ${hits.map((h) => {
                const matchedJob = jobByPath.get(h.path) || jobs.find((x) => h.path.endsWith("/" + jobFileName(x)));
                const suggested = h.suggested_creator_tag || h.normalized_text || h.raw_text;
                return `
                  <div class="dedupe-member-row">
                    <div class="min-w-0" style="flex:1;">
                      <div class="inline-row" style="gap:6px;flex-wrap:wrap;">
                        <strong style="font-size:12.5px;">${escapeHtml(h.normalized_text || h.raw_text)}</strong>
                        <span class="badge badge-ok">${Math.round((h.confidence || 0) * 100)}% conf</span>
                        <span class="badge">${escapeHtml(h.classification || "watermark")}</span>
                      </div>
                      <div class="mono muted truncate" style="font-size:11px;margin-top:2px;">${escapeHtml(h.path)} @ ${fmtDuration(h.timestamp_secs || 0)}</div>
                    </div>
                    <div class="inline-row" style="gap:6px;">
                      ${matchedJob ? `<button type="button" class="btn btn-sm btn-secondary" data-preview-job="${escapeHtml(matchedJob.id)}">Preview</button>` : ""}
                      ${matchedJob && suggested ? `<button type="button" class="btn btn-sm btn-primary" data-apply-wm-tag="${escapeHtml(matchedJob.id)}::${escapeHtml(suggested)}">Tag "${escapeHtml(suggested)}"</button>` : ""}
                    </div>
                  </div>
                `;
              }).join("")}
            </div>
          </div>
        ` : `
          <div class="setup-card" style="padding:20px;text-align:center;">
            <strong style="font-size:13px;">No watermarks extracted for these files yet</strong>
            <p class="muted" style="font-size:12px;margin:4px 0 0;">Click <strong>Extract Watermarks</strong> above, or apply custom tags directly to any file below.</p>
          </div>
        `}
        <div class="setup-card">
          <div class="setup-row-between" style="flex-wrap:wrap;gap:8px;">
            <span class="setup-kicker" style="margin:0;">Collection Files &amp; Tags (${jobs.length})</span>
            <div class="inline-row" style="gap:6px;">
              <input type="text" id="org-batch-tag-input" class="compact-sel" placeholder="Tag all / selected…" style="min-width:160px;" />
              <button type="button" id="org-batch-tag-btn" class="btn btn-sm btn-secondary">Apply Tag</button>
            </div>
          </div>
          <div class="sidebar-panel-stack" style="gap:6px;margin-top:8px;">
            ${jobs.map((j) => `
              <div class="inline-row" style="justify-content:space-between;padding:8px 10px;border-radius:9px;background:var(--surface-2);border:1px solid var(--border);gap:10px;">
                <div class="min-w-0" style="flex:1;">
                  <div class="inline-row" style="gap:6px;">
                    <input type="checkbox" class="org-del-chk" data-org-del-id="${escapeHtml(j.id)}" ${orgSel.has(j.id) ? "checked" : ""} />
                    <strong style="font-size:12.5px;" class="truncate">${escapeHtml(jobFileName(j))}</strong>
                  </div>
                  ${renderJobTagsHtml(j.id, ov.job_tags?.[j.id] || [])}
                </div>
                <button type="button" class="btn btn-sm btn-secondary" data-preview-job="${escapeHtml(j.id)}">Preview</button>
              </div>
            `).join("")}
          </div>
        </div>
      `;
    } else if (organizeSideTab === "faces") {
      const clusters = ov.face_groups || [];
      mainHtml = `
        <div class="summary-hero-card">
          <div>
            <strong style="font-size:14px;">People &amp; Face Grouping</strong>
            <div class="muted" style="font-size:12px;margin-top:3px;">Detects &amp; clusters faces locally on your Mac. Name a person once to automatically apply their <code class="mono">person:Name</code> tag to all matching clips.</div>
          </div>
          <div class="inline-row" style="gap:8px;">
            <button type="button" id="org-face-scan" class="btn btn-sm btn-primary" ${faceScanning || jobs.length === 0 ? "disabled" : ""}>${faceScanning ? "Detecting Faces…" : "Detect People"}</button>
          </div>
        </div>
        ${clusters.length === 0 ? `
          <div class="setup-card" style="padding:24px;text-align:center;">
            <strong style="font-size:13px;">No face clusters detected in this tab yet</strong>
            <p class="muted" style="font-size:12px;margin:4px 0 0;">Click <strong>Detect People</strong> above to sample frames &amp; group recurring faces privately offline.</p>
          </div>
        ` : `
          <div class="face-cluster-grid">
            ${clusters.map((grp) => `
              <div class="face-cluster-card">
                <div class="inline-row" style="gap:10px;">
                  <img class="face-crop-thumb" data-crop-path="${escapeHtml(grp.sample_crop_path || "")}" alt="Face sample" />
                  <div class="min-w-0" style="flex:1;">
                    <div style="font-size:12.5px;font-weight:700;">${escapeHtml(grp.label || "Unnamed Person")}</div>
                    <div class="muted tabular-nums" style="font-size:11px;">${grp.observation_count} sighting${grp.observation_count === 1 ? "" : "s"} · ${grp.matched_job_ids.length} file${grp.matched_job_ids.length === 1 ? "" : "s"}</div>
                  </div>
                </div>
                <div class="inline-row" style="gap:6px;">
                  <input type="text" class="full-input" id="face-name-${escapeHtml(grp.group_id)}" placeholder="Enter person name…" value="${escapeHtml(grp.label || "")}" />
                  <button type="button" class="btn btn-sm btn-primary" data-label-face="${escapeHtml(grp.group_id)}">Name &amp; Tag</button>
                </div>
                <div class="inline-row" style="gap:4px;flex-wrap:wrap;">
                  ${grp.matched_job_ids.slice(0, 5).map((jid) => {
                    const mj = jobById.get(jid);
                    return mj ? `<button type="button" class="tag-chip" style="cursor:pointer;" data-preview-job="${escapeHtml(mj.id)}">${escapeHtml(jobFileName(mj))}</button>` : "";
                  }).join("")}
                </div>
              </div>
            `).join("")}
          </div>
        `}
      `;
    } else if (organizeSideTab === "folder") {
      const availTags = ov.available_tags || [];
      if (!moveFolderTargetDir && q.destination_root) {
        moveFolderTargetDir = q.destination_root;
      }
      const prevRes = movePreviewResult && movePreviewResult._queueId === q.id ? movePreviewResult : null;
      const validMoveCount = prevRes?.items?.filter((x) => x.valid).length || 0;

      mainHtml = `
        <div class="summary-hero-card">
          <div>
            <strong style="font-size:14px;">Sort Files Into Folder by Tag</strong>
            <div class="muted" style="font-size:12px;margin-top:3px;">Pick any watermark, person, or custom tag and move (or copy) matching files into a target folder using a reviewed, atomic plan.</div>
          </div>
        </div>
        <div class="setup-card">
          <span class="setup-kicker">1. Pick Tag &amp; Destination Folder</span>
          ${availTags.length > 0 ? `
            <div class="inline-row" style="gap:6px;flex-wrap:wrap;">
              ${availTags.map((t) => `<button type="button" class="pill-filter-btn ${moveSelectedTag === t ? "active" : ""}" data-pick-move-tag="${escapeHtml(t)}">${escapeHtml(t)}</button>`).join("")}
            </div>
          ` : `<p class="muted" style="font-size:12px;margin:0;">No tags found on files in this tab yet. Add tags in Largest, Watermarks, or People first.</p>`}
          <div class="seg-pair-grid">
            <div class="field-block">
              <label class="field-label" for="org-move-tag">Selected Tag</label>
              <input id="org-move-tag" class="full-input" type="text" value="${escapeHtml(moveSelectedTag)}" placeholder="e.g. person:Alex or channel..." />
            </div>
            <div class="field-block">
              <label class="field-label" for="org-move-dest">Destination Folder</label>
              <div class="inline-row" style="gap:6px;">
                <input id="org-move-dest" class="full-input mono" type="text" value="${escapeHtml(moveFolderTargetDir)}" placeholder="/path/to/target/folder" />
                <button type="button" id="org-move-browse" class="btn btn-sm btn-secondary">Browse…</button>
              </div>
            </div>
          </div>
          <div class="seg-pair-grid" style="margin-top:4px;">
            <label class="check-row" style="gap:8px;"><input type="checkbox" id="org-move-subfolders" class="switch-chk" ${movePreserveSubfolders ? "checked" : ""} /><span>Preserve original subfolders inside target</span></label>
            <div class="field-block">
              <label class="field-label" for="org-move-collision">Filename Collisions</label>
              <select id="org-move-collision" class="compact-sel" style="width:100%;">
                <option value="keep_existing" ${moveCollisionPolicy === "keep_existing" ? "selected" : ""}>Keep existing (skip &amp; warn — safest)</option>
                <option value="number" ${moveCollisionPolicy === "number" ? "selected" : ""}>Auto-number duplicates (e.g. clip (1).mp4)</option>
                <option value="skip" ${moveCollisionPolicy === "skip" ? "selected" : ""}>Skip conflicting files silently</option>
              </select>
            </div>
          </div>
          <div class="setup-row-between" style="flex-wrap:wrap;gap:8px;">
            <label class="check-row" style="gap:8px;"><input type="checkbox" id="org-move-copy" class="switch-chk" ${moveCopyOnly ? "checked" : ""} /><span>Copy files instead of moving them</span></label>
            <div class="inline-row" style="gap:8px;">
              <button type="button" id="org-move-preview" class="btn btn-sm btn-secondary" ${!moveSelectedTag || !moveFolderTargetDir ? "disabled" : ""}>Preview Matching Files</button>
              ${prevRes && validMoveCount > 0 && prevRes.plan_id ? `<button type="button" id="org-move-exec" class="btn btn-sm btn-primary">${moveCopyOnly ? "Copy" : "Move"} ${validMoveCount} Matching File(s)</button>` : ""}
            </div>
          </div>
        </div>
        ${prevRes ? `
          <div class="setup-card">
            <span class="setup-kicker">Preview (${prevRes.items.length} file${prevRes.items.length === 1 ? "" : "s"} matched "${escapeHtml(prevRes.tag)}")</span>
            ${prevRes.items.length === 0 ? `<p class="muted" style="font-size:12px;margin:0;">No files in this tab carry the tag "${escapeHtml(prevRes.tag)}".</p>` : `
              <table class="table-simple">
                <thead><tr><th>File</th><th>Destination</th><th>Size</th><th>Status</th></tr></thead>
                <tbody>
                  ${prevRes.items.map((it) => `
                    <tr>
                      <td><strong>${escapeHtml(it.filename)}</strong>${it.cross_volume ? ` <span class="badge">Cross-vol</span>` : ""}</td>
                      <td class="mono muted truncate" style="max-width:260px;">${escapeHtml(it.new_path)}</td>
                      <td class="tabular-nums">${fmtBytes(it.size_bytes)}</td>
                      <td>${it.copied_original_retained ? `<span class="badge badge-warn">Copied; original retained</span>` : it.valid ? `<span class="badge badge-ok">Ready</span>` : `<span class="badge badge-err">${escapeHtml(it.error || "Skipped")}</span>`}</td>
                    </tr>
                  `).join("")}
                </tbody>
              </table>
            `}
          </div>
        ` : ""}
      `;
    }

    tabPane.innerHTML = mainHtml;

    // Wire selection checkboxes
    el.querySelectorAll(".org-del-chk").forEach((chk) => {
      chk.onchange = () => {
        const jid = chk.getAttribute("data-org-del-id");
        const s = getOrgSelectedSet(q.id);
        if (chk.checked) s.add(jid);
        else s.delete(jid);
        renderOrganizeView(q, jobs);
        renderActionBar();
      };
    });

    // Wire preview buttons (both images and videos)
    el.querySelectorAll("[data-preview-job]").forEach((btn) => {
      btn.onclick = (ev) => {
        ev.stopPropagation();
        const j = jobById.get(btn.getAttribute("data-preview-job"));
        if (j) openMediaPreviewModal(j);
      };
    });

    // Wire selecting a job card in Largest view
    el.querySelectorAll("[data-org-job-card]").forEach((card) => {
      card.onclick = (ev) => {
        if (ev.target.closest("button, input")) return;
        selectedJobId = card.getAttribute("data-org-job-card");
        renderOrganizeView(q, jobs);
        renderSidebar();
      };
    });

    // Wire untag / add-tag
    el.querySelectorAll("[data-untag]").forEach((btn) => {
      btn.onclick = async (ev) => {
        ev.stopPropagation();
        const [jobId, ...rest] = btn.getAttribute("data-untag").split("::");
        await invoke("tab_untag_resource", { jobId, tagName: rest.join("::") });
        await loadOrganizeOverview(q.id, true);
      };
    });
    el.querySelectorAll("[data-add-tag-job]").forEach((btn) => {
      btn.onclick = (ev) => {
        ev.stopPropagation();
        const jobId = btn.getAttribute("data-add-tag-job");
        openPromptModal("Add Tag", "Tag name:", "", async (val) => {
          await invoke("tab_tag_resources", { jobIds: [jobId], tagNames: [val] });
          await loadOrganizeOverview(q.id, true);
        });
      };
    });

    // Side-tab-specific handlers
    el.querySelector("#org-fwd-largest")?.addEventListener("click", () => {
      const videoJobs = jobs.filter((j) => !isImagePath(j.source_path));
      openForwardModal(videoJobs.map((j) => j.id), "encode", "videos from Organize", false);
    });

    el.querySelector("#org-trash-selected")?.addEventListener("click", () => trashOrgSelected(q));

    el.querySelector("#org-select-redundant")?.addEventListener("click", () => {
      const s = getOrgSelectedSet(q.id);
      groups
        .filter((g) => String(g.match_type || "").startsWith("Exact"))
        .flatMap((g) => g.items)
        .filter((i) => i.role === "trash" && !i.trashed)
        .forEach((i) => {
          const j = jobById.get(i.job_id);
          if (j && !isProtectedSource(j.source_path)) s.add(i.job_id);
        });
      renderOrganizeView(q, jobs);
      renderActionBar();
    });

    el.querySelector("#btn-tab-dedupe-scan")?.addEventListener("click", () => triggerTabDedupeScan(q));

    el.querySelector("#org-clear-cache")?.addEventListener("click", async () => {
      const removed = await invoke("clear_hash_cache");
      showToast(`Deleted ${removed} cached SQLite file hash entr${removed === 1 ? "y" : "ies"}.`, "info");
      await loadOrganizeOverview(q.id, true);
    });

    el.querySelector("#org-ocr-scan")?.addEventListener("click", () => triggerTabWatermarkScan(q));

    el.querySelectorAll("[data-apply-wm-tag]").forEach((btn) => {
      btn.onclick = async () => {
        const [jobId, ...rest] = btn.getAttribute("data-apply-wm-tag").split("::");
        const tag = rest.join("::");
        await invoke("tab_tag_resources", { jobIds: [jobId], tagNames: [tag] });
        showToast(`Tagged file with "${tag}"`, "success");
        await loadOrganizeOverview(q.id, true);
      };
    });

    el.querySelector("#org-batch-tag-btn")?.addEventListener("click", async () => {
      const val = el.querySelector("#org-batch-tag-input")?.value.trim();
      if (!val) return showToast("Enter a tag name first.", "warn");
      const s = getOrgSelectedSet(q.id);
      const targets = s.size > 0 ? Array.from(s) : jobs.map((j) => j.id);
      const count = await invoke("tab_tag_resources", { jobIds: targets, tagNames: [val] });
      showToast(`Tagged ${count} item(s) with "${val}".`, "success");
      await loadOrganizeOverview(q.id, true);
    });

    el.querySelector("#org-face-scan")?.addEventListener("click", () => triggerTabFaceScan(q));

    el.querySelectorAll("[data-crop-path]").forEach((imgEl) => {
      const cp = imgEl.getAttribute("data-crop-path");
      if (!cp) return;
      loadLocalImageUrl(cp).then((url) => { imgEl.src = url; }).catch(() => {});
    });

    el.querySelectorAll("[data-label-face]").forEach((btn) => {
      btn.onclick = async () => {
        const gid = btn.getAttribute("data-label-face");
        const inp = el.querySelector(`#face-name-${CSS.escape(gid)}`);
        const label = inp?.value.trim();
        if (!label) return showToast("Enter a person name first.", "warn");
        const tagged = await invoke("tab_label_face_and_tag", { queueId: q.id, groupId: gid, label });
        showToast(`Named "${label}" and tagged ${tagged} matching file(s).`, "success");
        await loadOrganizeOverview(q.id, true);
      };
    });

    el.querySelectorAll("[data-pick-move-tag]").forEach((btn) => {
      btn.onclick = () => {
        moveSelectedTag = btn.getAttribute("data-pick-move-tag");
        movePreviewResult = null;
        renderOrganizeView(q, jobs);
      };
    });

    el.querySelector("#org-move-tag")?.addEventListener("input", (e) => {
      moveSelectedTag = e.target.value;
      movePreviewResult = null;
      const prevBtn = el.querySelector("#org-move-preview");
      if (prevBtn) prevBtn.disabled = !moveSelectedTag.trim() || !moveFolderTargetDir.trim();
      const execBtn = el.querySelector("#org-move-exec");
      if (execBtn) execBtn.disabled = true;
    });

    el.querySelector("#org-move-dest")?.addEventListener("input", (e) => {
      moveFolderTargetDir = e.target.value;
      movePreviewResult = null;
      const prevBtn = el.querySelector("#org-move-preview");
      if (prevBtn) prevBtn.disabled = !moveSelectedTag.trim() || !moveFolderTargetDir.trim();
      const execBtn = el.querySelector("#org-move-exec");
      if (execBtn) execBtn.disabled = true;
    });

    el.querySelector("#org-move-browse")?.addEventListener("click", async () => {
      const dir = await dialog.open({ directory: true, defaultPath: moveFolderTargetDir || q.destination_root || undefined });
      if (dir) {
        moveFolderTargetDir = Array.isArray(dir) ? dir[0] : dir;
        movePreviewResult = null;
        renderOrganizeView(q, jobs);
      }
    });

    el.querySelector("#org-move-copy")?.addEventListener("change", (e) => {
      moveCopyOnly = e.target.checked;
      movePreviewResult = null;
      renderOrganizeView(q, jobs);
    });

    el.querySelector("#org-move-subfolders")?.addEventListener("change", (e) => {
      movePreserveSubfolders = e.target.checked;
      movePreviewResult = null;
      renderOrganizeView(q, jobs);
    });

    el.querySelector("#org-move-collision")?.addEventListener("change", (e) => {
      moveCollisionPolicy = e.target.value;
      movePreviewResult = null;
      renderOrganizeView(q, jobs);
    });

    el.querySelector("#org-move-preview")?.addEventListener("click", () => runMoveByTag(q, true));
    el.querySelector("#org-move-exec")?.addEventListener("click", () => runMoveByTag(q, false));
  }

  function renderRepairView(q, jobs) {
    const el = $("mode-view-repair");
    const repairedJobs = jobs.filter((j) => {
      const r = snapshot.tab_repair_items?.[j.id];
      return r && ["recovered", "partial_recovery"].includes(r.status) && r.output_path;
    });

    el.innerHTML = `
      <div class="summary-hero-card">
        <div>
          <strong style="font-size:14px;">Non-Destructive Video Diagnosis &amp; Bitstream Recovery</strong>
          <div class="muted" style="font-size:12px;margin-top:3px;">Inspects MP4/MOV atoms, matches reference parameters, and writes a fresh recovered file without altering damaged originals.</div>
        </div>
        ${repairedJobs.length > 0 ? `<button type="button" id="btn-repair-fwd-all" class="btn btn-sm btn-primary">Forward ${repairedJobs.length} Repaired → Transcode</button>` : ""}
      </div>
      <div id="repair-cards-list" class="sidebar-panel-stack"></div>
    `;

    el.querySelector("#btn-repair-fwd-all")?.addEventListener("click", () => {
      openForwardModal(repairedJobs.map((j) => j.id), "encode", "verified repaired outputs", true);
    });

    const list = el.querySelector("#repair-cards-list");
    for (const j of jobs) {
      const rState = snapshot.tab_repair_items?.[j.id];
      const st = rState?.status || "idle";
      const isDone = ["recovered", "partial_recovery"].includes(st);
      const isBusy = st === "diagnosing" || st === "recovering";
      const lp = repairLiveProgress.get(j.id);

      const badgeCls = isDone ? "badge-ok" : isBusy ? "badge-active" : st === "failed" || st === "unrecoverable" ? "badge-err" : "badge-warn";
      const stText = {
        idle: "Ready to Diagnose / Repair",
        diagnosing: "Diagnosing container…",
        diagnosed: "Diagnosed",
        recovering: "Recovering streams…",
        recovered: "Repaired & Verified",
        partial_recovery: "Partial Recovery",
        failed: "Recovery Failed",
        unrecoverable: "Unrecoverable",
      }[st] || st;

      const diagSummary = rState?.diagnosis
        ? `${rState.diagnosis.detected_issues?.[0] || "Container analyzed"} · Size: ${fmtBytes(rState.diagnosis.actual_file_size)}`
        : (j.error_message || "Click Diagnose or Run Repair to analyze MP4/MOV atoms and streams");

      const card = document.createElement("article");
      card.className = `repair-item-card ${j.id === selectedJobId ? "selected" : ""}`;
      card.innerHTML = `
        <div class="inline-row" style="justify-content:space-between;gap:12px;flex-wrap:wrap;">
          <div class="min-w-0" style="flex:1;">
            <div class="inline-row" style="gap:7px;flex-wrap:wrap;">
              <strong style="font-size:13px;">${escapeHtml(jobFileName(j))}</strong>
              <span class="badge ${badgeCls}">${escapeHtml(stText)}</span>
            </div>
            <div class="muted" style="font-size:11.5px;margin-top:4px;">${escapeHtml(diagSummary)}</div>
            ${rState?.output_path ? `<div style="font-size:11px;color:var(--ok);margin-top:4px;" class="mono truncate">Output: ${escapeHtml(rState.output_path)} (${fmtBytes(rState.output_bytes || 0)})</div>` : ""}
            ${rState?.error ? `<div style="font-size:11px;color:var(--danger);margin-top:4px;">${escapeHtml(rState.error)}</div>` : ""}
          </div>
          <div class="inline-row" style="gap:6px;flex-shrink:0;">
            ${isDone && rState?.output_path ? `
              <button type="button" class="btn btn-sm btn-secondary" data-show-rep="${j.id}">Show</button>
              <button type="button" class="accent-pill-btn" data-fwd-rep="${j.id}">Forward Output →</button>
            ` : `
              <button type="button" class="btn btn-sm btn-secondary" data-diag="${j.id}" ${isBusy ? "disabled" : ""}>Diagnose</button>
              <button type="button" class="btn btn-sm btn-primary" data-run-rep="${j.id}" ${isBusy ? "disabled" : ""}>Run Repair</button>
            `}
            <button type="button" class="btn btn-ghost btn-sm" data-rm="${j.id}" title="Remove from tab">×</button>
          </div>
        </div>
        ${isBusy && lp ? `
          <div class="progress-wrap" style="margin-top:8px;">
            <div class="inline-row" style="justify-content:space-between;font-size:11px;margin-bottom:3px;">
              <span>${escapeHtml(lp.stage || "Working…")}</span>
              <span class="mono muted">${Math.round((lp.fraction || 0) * 100)}%</span>
            </div>
            <div class="progress-track"><div class="progress-fill" style="width:${Math.round((lp.fraction || 0) * 100)}%"></div></div>
          </div>
        ` : ""}
      `;

      card.addEventListener("click", (e) => {
        if (e.target.closest("button")) return;
        selectedJobId = j.id;
        sidebarMode = "job";
        sidebarOpen = true;
        renderAll();
      });
      card.querySelector("[data-diag]")?.addEventListener("click", (e) => {
        e.stopPropagation();
        selectedJobId = j.id;
        sidebarMode = "job";
        invoke("run_tab_repair_diagnosis", { queueId: q.id, jobId: j.id });
      });
      card.querySelector("[data-run-rep]")?.addEventListener("click", (e) => {
        e.stopPropagation();
        selectedJobId = j.id;
        sidebarMode = "job";
        invoke("run_tab_repair_execute", { queueId: q.id, jobIds: [j.id] });
      });
      card.querySelector("[data-show-rep]")?.addEventListener("click", (e) => {
        e.stopPropagation();
        if (rState?.output_path) invoke("reveal_output", { path: rState.output_path });
      });
      card.querySelector("[data-fwd-rep]")?.addEventListener("click", (e) => {
        e.stopPropagation();
        openForwardModal([j.id], "encode", "repaired output", true);
      });
      card.querySelector("[data-rm]").onclick = (e) => {
        e.stopPropagation();
        invoke("remove_pending_jobs", { jobIds: [j.id] });
      };

      list.appendChild(card);
    }
  }

  // =========================================================================
  // 5. CONTEXTUAL RIGHT SIDEBAR
  // =========================================================================
  function renderSidebar() {
    const sp = $("side-panel");
    sp.classList.toggle("collapsed", !sidebarOpen);
    $("btn-toggle-inspector").setAttribute("aria-expanded", String(sidebarOpen));
    if (!sidebarOpen) return;

    const q = activeQueue();
    if (!q) return;
    const mode = activeMode(q);
    const jobs = activeQueueJobs();

    const labels = {
      encode: ["Queue Setup", "Job Details"],
      explore: ["Tab Resources", "Inspect & Decide"],
      organize: ["Organize Rules", "Tags & Preview"],
      repair: ["Repair Policy", "Diagnosis"],
    }[mode] || ["Setup", "Details"];

    $("sidebar-tab-setup").textContent = labels[0];
    $("sidebar-tab-job").textContent = labels[1];
    $("sidebar-tab-setup").classList.toggle("active", sidebarMode === "setup");
    $("sidebar-tab-job").classList.toggle("active", sidebarMode === "job");

    $("panel-queue-setup").classList.add("hidden");
    $("side-panel-content").classList.add("hidden");
    $("panel-mode-config").classList.add("hidden");
    $("panel-mode-inspect").classList.add("hidden");

    if (sidebarMode === "setup") {
      if (mode === "encode") {
        $("panel-queue-setup").classList.remove("hidden");
        renderQueueSetupPanel(q, jobs);
      } else {
        $("panel-mode-config").classList.remove("hidden");
        renderModeConfigPanel(q, mode, jobs);
      }
    } else {
      if (mode === "encode") {
        $("side-panel-content").classList.remove("hidden");
        renderEncodeJobDetailsPanel(q, jobs);
      } else {
        $("panel-mode-inspect").classList.remove("hidden");
        renderModeInspectPanel(q, mode, jobs);
      }
    }
  }

  function renderQueueSetupPanel(q, jobs) {
    const selProf = $("sel-profile");
    selProf.innerHTML = `<option value="">Custom / Queue Settings</option>`;
    for (const p of snapshot.profiles) {
      const opt = document.createElement("option");
      opt.value = p.id;
      opt.textContent = p.name;
      if (q.profile_id === p.id) opt.selected = true;
      selProf.appendChild(opt);
    }

    const checkedWait = jobs.filter((j) => checkedJobIds.has(j.id) && ["queued", "ready", "needs_attention", "interrupted", "failed", "canceled"].includes(j.status)).length;
    const waitCount = jobs.filter((j) => ["queued", "ready", "needs_attention", "interrupted", "failed", "canceled"].includes(j.status)).length;
    $("btn-apply-waiting").textContent = checkedWait > 0 ? `Apply to ${checkedWait} selected` : `Apply to ${waitCount} waiting`;
    $("btn-apply-waiting").disabled = waitCount === 0;

    const r = q.recipe;
    $("sel-family").value = r.family;
    $("sel-variant").value = r.variant;
    $("sel-codec").value = r.codec;
    $("sel-container").value = r.container;

    document.querySelectorAll("[data-seg]").forEach((grp) => {
      const key = grp.getAttribute("data-seg");
      grp.querySelectorAll(".seg-btn").forEach((btn) => {
        btn.classList.toggle("active", btn.getAttribute("data-val") === r[key]);
      });
    });

    const isRemux = r.family === "remux";
    $("wrap-encode-controls").classList.toggle("hidden", isRemux);
    $("badge-custom").classList.toggle("hidden", !r.is_custom);
    $("btn-reset-preset").classList.toggle("hidden", !r.is_custom);

    const crfMax = r.codec === "hevc" ? 51 : 63;
    $("lbl-crf-range").textContent = `0–${crfMax}`;
    $("inp-crf").max = crfMax;
    $("inp-crf").value = r.crf;
    $("rng-crf").max = crfMax;
    $("rng-crf").value = r.crf;

    const efforts = r.codec === "hevc" ? ["medium", "slow", "slower", "veryslow"] : ["3", "4", "5", "6"];
    const effPills = $("effort-pills");
    effPills.innerHTML = "";
    const selEff = $("sel-effort");
    selEff.innerHTML = "";
    for (const ef of efforts) {
      const opt = document.createElement("option");
      opt.value = ef;
      opt.textContent = ef;
      if (String(r.effort) === ef) opt.selected = true;
      selEff.appendChild(opt);

      const b = document.createElement("button");
      b.type = "button";
      b.className = "seg-btn" + (String(r.effort) === ef ? " active" : "");
      b.textContent = r.codec === "av1" ? `P${ef}` : ef.charAt(0).toUpperCase() + ef.slice(1);
      b.onclick = () => patchActiveQueueSettings((p) => { p.recipe.effort = ef; });
      effPills.appendChild(b);
    }

    const crfHelp = $("help-crf-box");
    if (crfHelp) {
      crfHelp.innerHTML = r.codec === "hevc"
        ? `<strong>HEVC (x265) CRF guide:</strong><br>• <strong>16–18</strong>: Archival / near-lossless (Anime BD, masters)<br>• <strong>19–21</strong>: Balanced sweet spot (sharp lines, big savings)<br>• <strong>22–24</strong>: Compact space-saver (slight softness)`
        : `<strong>AV1 (SVT-AV1) CRF guide:</strong><br>• <strong>20–24</strong>: Archival / high detail<br>• <strong>25–29</strong>: Balanced everyday compression<br>• <strong>30–34</strong>: Compact library storage`;
    }

    if ($("sel-tune")) {
      const isAv1 = r.codec === "av1";
      $("sel-tune").disabled = isAv1;
      $("sel-tune").value = isAv1 ? "" : (r.tune || "");
      $("sel-aq-mode").disabled = isAv1;
      $("sel-aq-mode").value = isAv1 ? "" : (r.aq_mode ? String(r.aq_mode) : "");
      $("chk-ten-bit").checked = Boolean(r.ten_bit);
      $("btn-anime-preset-quick").classList.toggle("hidden", isAv1);
    }

    const selNp = $("sel-named-preset");
    const prevNp = selNp.value;
    selNp.innerHTML = `<option value="">Load saved preset…</option>`;
    for (const np of snapshot.named_presets) {
      const opt = document.createElement("option");
      opt.value = np.id;
      opt.textContent = np.name;
      if (prevNp === np.id) opt.selected = true;
      selNp.appendChild(opt);
    }
    $("btn-delete-named-preset").classList.toggle("hidden", !selNp.value);

    $("lbl-dest-root").textContent = q.destination_root || "Same folder as source";
    const vol = findQueueVolumeInfo(q);
    $("lbl-dest-space").textContent = vol ? `${fmtBytes(vol.free_bytes)} free (${vol.mount_point})` : "Auto";
    if (vol && vol.total_bytes > 0) {
      const usedPct = Math.min(100, Math.max(4, Math.round(((vol.total_bytes - vol.free_bytes) / vol.total_bytes) * 100)));
      $("disk-bar-fill").style.width = `${usedPct}%`;
    }

    $("sel-cleanup-policy").value = q.cleanup_policy;
    $("chk-min-savings").checked = q.min_savings_enabled;
    $("inp-min-savings-pct").value = q.min_savings_pct;
    $("sel-priority").value = q.priority;
    $("sel-error-policy").value = q.error_policy;
    $("chk-recursive").checked = q.recursive_import;
    $("chk-keep-awake").checked = q.keep_awake;
    $("chk-full-decode").checked = q.full_decode_verify;
    $("lbl-advanced-summary").textContent = `${q.priority} · ${q.keep_awake ? "Keep awake" : "Allow sleep"} · ${q.error_policy === "continue" ? "Continue on error" : "Pause on error"}`;
  }

  function renderEncodeJobDetailsPanel(q, jobs) {
    const el = $("side-panel-content");
    const j = jobs.find((x) => x.id === selectedJobId) || jobs[0];
    if (!j) {
      el.innerHTML = `<div class="setup-card" style="text-align:center;padding:24px;"><span class="muted" style="font-size:12px;">Select a file to inspect its stream plan, output path, and FFmpeg command.</span></div>`;
      return;
    }

    const info = jobVideoInfo(j);
    el.innerHTML = `
      <div class="setup-card">
        <span class="setup-kicker">Selected Job</span>
        <div style="font-size:12.5px;font-weight:600;word-break:break-all;">${escapeHtml(jobFileName(j))}</div>
        <div class="muted tabular-nums" style="font-size:11px;">${fmtBytes(j.input_bytes)} · ${fmtDuration(info.dur)} · ${escapeHtml(info.codec)} ${info.mbps > 0 ? info.mbps + " Mbps" : ""}</div>
        <div class="inline-row" style="gap:6px;">
          <button type="button" id="jd-preview" class="accent-pill-btn" style="flex:1;">Preview 15s</button>
          ${j.status === "encoding"
            ? `<button type="button" id="jd-live" class="btn btn-sm btn-secondary" style="flex:1;">Live View</button>`
            : `<button type="button" id="jd-fwd" class="btn btn-sm btn-secondary" style="flex:1;">Forward →</button>`}
        </div>
      </div>

      <div class="setup-card">
        <div class="setup-card-head">
          <span class="setup-kicker">Planned Output Path</span>
          ${["queued", "ready", "needs_attention", "interrupted", "failed", "canceled"].includes(j.status) ? `<button type="button" id="jd-dest" class="btn-link">Change</button>` : ""}
        </div>
        <div class="mono muted" style="font-size:11px;word-break:break-all;">${escapeHtml(j.destination_path)}</div>
      </div>

      ${j.plan ? `
        <div class="setup-card">
          <span class="setup-kicker">Explicit Stream Plan (${j.plan.tracks.length} tracks)</span>
          <div class="sidebar-panel-stack" style="gap:5px;">
            ${j.plan.tracks.map((t) => `
              <div style="font-size:11px;padding:6px 8px;border-radius:7px;background:var(--surface);border:1px solid var(--border);">
                <span class="mono">#${t.stream_index}</span> <strong>${escapeHtml(t.codec_type)}</strong> (${escapeHtml(t.codec_name)}) → ${escapeHtml(t.action)}
              </div>
            `).join("")}
          </div>
        </div>
      ` : ""}

      <div class="setup-card">
        <span class="setup-kicker">FFmpeg Command</span>
        <pre class="mono muted" style="font-size:10px;white-space:pre-wrap;word-break:break-all;margin:0;">${escapeHtml(j.plan?.printable_command || "Will be planned before encode")}</pre>
      </div>
    `;

    el.querySelector("#jd-preview").onclick = () => openSamplePreviewModal(j);
    el.querySelector("#jd-live")?.addEventListener("click", () => openLivePreviewModal(j.id));
    el.querySelector("#jd-fwd")?.addEventListener("click", () => {
      openForwardModal([j.id], j.status === "needs_attention" ? "repair" : "explore", "selected file", j.status === "completed");
    });
    el.querySelector("#jd-dest")?.addEventListener("click", async () => {
      const picked = await dialog.save({ defaultPath: j.destination_path });
      if (picked) await invoke("update_pending_job", { jobId: j.id, patch: { destination_path: picked } });
    });
  }

  function renderModeConfigPanel(q, mode, jobs) {
    const el = $("panel-mode-config");
    const meta = activeTabMeta(q);

    if (mode === "explore") {
      const totalB = jobs.reduce((a, b) => a + (b.input_bytes || 0), 0);
      const protB = jobs.filter((j) => jobDecision(j) === "protected").reduce((a, b) => a + (b.input_bytes || 0), 0);
      const compB = jobs.filter((j) => ["compress_balanced", "compress_compact"].includes(jobDecision(j))).reduce((a, b) => a + (b.input_bytes || 0), 0);
      el.innerHTML = `
        <div class="setup-card">
          <span class="setup-kicker">Resource Bundle Summary</span>
          <div class="mono muted" style="font-size:11px;word-break:break-all;background:var(--surface);padding:7px;border-radius:7px;border:1px solid var(--border);">${escapeHtml(meta.resource_source || `${jobs.length} files attached`)}</div>
          <div class="seg-pair-grid tabular-nums">
            <div style="padding:7px;border-radius:8px;background:var(--surface);border:1px solid var(--border);">
              <div class="muted" style="font-size:10px;">Total Scanned</div>
              <div style="font-size:12.5px;font-weight:700;">${fmtBytes(totalB)}</div>
            </div>
            <div style="padding:7px;border-radius:8px;background:var(--surface);border:1px solid var(--border);">
              <div style="font-size:10px;color:var(--warn);">Protected ★</div>
              <div style="font-size:12.5px;font-weight:700;">${fmtBytes(protB)}</div>
            </div>
          </div>
          <div style="padding:8px;border-radius:8px;background:rgba(16,185,129,0.1);border:1px solid rgba(16,185,129,0.28);">
            <div style="font-size:10px;color:var(--ok);">Tagged for Compression</div>
            <div style="font-size:12px;font-weight:600;">${fmtBytes(compB)} selected</div>
          </div>
        </div>
        <div class="setup-card">
          <span class="setup-kicker">Pipeline Tip</span>
          <p class="muted" style="font-size:11.5px;line-height:1.45;margin:0;">Click any video to load real extracted storyboard frames in <strong>Inspect &amp; Decide</strong>. Mark key masters <strong>★ Keep</strong>, tag the rest <strong>Balanced</strong> or <strong>Compact</strong>, then click <strong>Forward →</strong> to send them straight to a Transcode tab.</p>
        </div>
      `;
    } else if (mode === "organize") {
      const ov = organizeOverview && organizeOverviewQueueId === q.id ? organizeOverview : null;
      const cacheStats = ov?.hash_cache || snapshot.hash_cache_stats || { entry_count: 0, total_hashed_bytes: 0, cache_bytes_estimate: 0 };
      const tags = ov?.available_tags || [];
      el.innerHTML = `
        <div class="setup-card">
          <span class="setup-kicker">Duplicate &amp; Similar Scan Rules</span>
          <label class="check-row-card" style="cursor:pointer;"><span class="check-title">Exact BLAKE3 + SHA-256 hash</span><input type="checkbox" id="cfg-ded-sha" class="switch-chk" ${dedupeAlsoSha256 ? "checked" : ""} /></label>
          <label class="check-row-card" style="cursor:pointer;"><span class="check-title">Perceptual match (≥88%)</span><input type="checkbox" id="cfg-ded-perc" class="switch-chk" ${dedupePerceptual ? "checked" : ""} /></label>
          <button type="button" id="cfg-ded-run" class="btn btn-sm btn-primary" ${dedupeScanning || jobs.length < 2 ? "disabled" : ""}>Run Duplicate Scan</button>
        </div>
        <div class="setup-card">
          <span class="setup-kicker">SQLite Hash Cache</span>
          <div class="muted tabular-nums" style="font-size:11.5px;">${cacheStats.entry_count || 0} cached files · ${fmtBytes(cacheStats.total_hashed_bytes || 0)} indexed (${fmtBytes(cacheStats.cache_bytes_estimate || 0)})</div>
          <button type="button" id="cfg-clear-cache" class="btn btn-sm btn-secondary" ${(cacheStats.entry_count || 0) === 0 ? "disabled" : ""}>Delete Hash Cache</button>
        </div>
        <div class="setup-card">
          <span class="setup-kicker">Tags in This Tab (${tags.length})</span>
          <div class="inline-row" style="gap:4px;flex-wrap:wrap;">
            ${tags.length > 0 ? tags.map((t) => `<span class="tag-chip">${escapeHtml(t)}</span>`).join("") : `<span class="muted" style="font-size:11px;">No tags yet — extract watermarks, group faces, or add tags manually.</span>`}
          </div>
        </div>
      `;
      el.querySelector("#cfg-ded-sha").onchange = (e) => { dedupeAlsoSha256 = e.target.checked; };
      el.querySelector("#cfg-ded-perc").onchange = (e) => { dedupePerceptual = e.target.checked; };
      el.querySelector("#cfg-ded-run").onclick = () => triggerTabDedupeScan(q);
      el.querySelector("#cfg-clear-cache").onclick = async () => {
        const removed = await invoke("clear_hash_cache");
        showToast(`Deleted ${removed} cached SQLite file hash entr${removed === 1 ? "y" : "ies"}.`, "info");
        await loadOrganizeOverview(q.id, true);
      };
    } else {
      el.innerHTML = `
        <div class="setup-card">
          <span class="setup-kicker">Repair Policy &amp; References</span>
          <div class="field-block">
            <div class="setup-row-between">
              <span class="field-label">Reference Videos (${meta.repair_refs.length})</span>
              <button type="button" id="rp-add-ref" class="btn-link">+ Add ref…</button>
            </div>
            <div id="rp-refs-list" class="sidebar-panel-stack" style="gap:4px;">
              ${meta.repair_refs.length === 0 ? `<span class="muted" style="font-size:11px;">None (recommended if moov is missing)</span>` : meta.repair_refs.map((p, idx) => `
                <div class="inline-row" style="justify-content:space-between;padding:4px 7px;border-radius:6px;background:var(--surface);border:1px solid var(--border);font-size:11px;">
                  <span class="truncate mono">${escapeHtml(p.split(/[\\/]/).pop())}</span>
                  <button type="button" class="btn-link" data-rm-ref="${idx}">×</button>
                </div>
              `).join("")}
            </div>
          </div>
          <label class="check-row-card" style="cursor:pointer;"><span class="check-title">Deep bitstream &amp; candidate scan</span><input type="checkbox" id="rp-deep" class="switch-chk" ${meta.repair_deep_scan ? "checked" : ""} /></label>
          <label class="check-row-card" style="cursor:pointer;"><span class="check-title">Prioritize video-only (omit audio)</span><input type="checkbox" id="rp-vonly" class="switch-chk" ${meta.repair_video_only ? "checked" : ""} /></label>
          <div class="setup-row-between">
            <span class="field-label">Timeout (s)</span>
            <input type="number" id="rp-timeout" min="15" max="3600" value="${meta.repair_timeout_secs || 300}" style="width:72px;" />
          </div>
        </div>
      `;
      el.querySelector("#rp-add-ref").onclick = async () => {
        const res = await dialog.open({ multiple: true });
        if (!res) return;
        const nextRefs = [...meta.repair_refs, ...(Array.isArray(res) ? res : [res])];
        await invoke("set_tab_workspace_meta", { queueId: q.id, meta: { ...meta, repair_refs: Array.from(new Set(nextRefs)) } });
      };
      el.querySelectorAll("[data-rm-ref]").forEach((b) => {
        b.onclick = () => {
          const idx = Number(b.getAttribute("data-rm-ref"));
          const nextRefs = meta.repair_refs.filter((_, i) => i !== idx);
          invoke("set_tab_workspace_meta", { queueId: q.id, meta: { ...meta, repair_refs: nextRefs } });
        };
      });
      const saveRepairMeta = () => {
        invoke("set_tab_workspace_meta", {
          queueId: q.id,
          meta: {
            ...meta,
            repair_deep_scan: el.querySelector("#rp-deep").checked,
            repair_video_only: el.querySelector("#rp-vonly").checked,
            repair_timeout_secs: Number(el.querySelector("#rp-timeout").value) || 300,
          },
        });
      };
      el.querySelector("#rp-deep").onchange = saveRepairMeta;
      el.querySelector("#rp-vonly").onchange = saveRepairMeta;
      el.querySelector("#rp-timeout").onchange = saveRepairMeta;
    }
  }

  function renderModeInspectPanel(q, mode, jobs) {
    const el = $("panel-mode-inspect");
    const j = jobs.find((x) => x.id === selectedJobId) || jobs[0];

    if (mode === "organize") {
      if (!j) {
        el.innerHTML = `<div class="setup-card" style="text-align:center;padding:24px;"><span class="muted" style="font-size:12px;">Select a file in Organize to preview or edit tags.</span></div>`;
        return;
      }
      const tags = (organizeOverview && organizeOverviewQueueId === q.id ? organizeOverview.job_tags?.[j.id] : []) || [];
      el.innerHTML = `
        <div class="setup-card">
          <span class="setup-kicker">Selected Resource</span>
          <div style="font-size:13px;font-weight:700;">${escapeHtml(jobFileName(j))}</div>
          <div class="mono muted" style="font-size:11px;word-break:break-all;">${escapeHtml(j.source_path)}</div>
          <div class="muted tabular-nums" style="font-size:11.5px;">${fmtBytes(j.input_bytes)} · ${isImagePath(j.source_path) ? "Image" : `${jobVideoInfo(j).res} · ${jobVideoInfo(j).dur}`}</div>
          <button type="button" id="org-ins-preview" class="btn btn-sm btn-primary">Preview ${isImagePath(j.source_path) ? "Image" : "Video"}</button>
        </div>
        <div class="setup-card">
          <span class="setup-kicker">Tags (${tags.length})</span>
          <div class="inline-row" style="gap:4px;flex-wrap:wrap;">
            ${tags.length > 0 ? tags.map((t) => `<span class="tag-chip">${escapeHtml(t)}<button type="button" class="tag-chip-x" data-ins-untag="${escapeHtml(t)}">×</button></span>`).join("") : `<span class="muted" style="font-size:11px;">No tags yet.</span>`}
          </div>
          <div class="inline-row" style="gap:6px;margin-top:4px;">
            <input type="text" id="org-ins-tag-inp" class="full-input" placeholder="Add tag…" />
            <button type="button" id="org-ins-tag-add" class="btn btn-sm btn-secondary">Add</button>
          </div>
        </div>
      `;
      el.querySelector("#org-ins-preview").onclick = () => openMediaPreviewModal(j);
      el.querySelectorAll("[data-ins-untag]").forEach((b) => {
        b.onclick = async () => {
          await invoke("tab_untag_resource", { jobId: j.id, tagName: b.getAttribute("data-ins-untag") });
          await loadOrganizeOverview(q.id, true);
        };
      });
      el.querySelector("#org-ins-tag-add").onclick = async () => {
        const val = el.querySelector("#org-ins-tag-inp").value.trim();
        if (!val) return;
        await invoke("tab_tag_resources", { jobIds: [j.id], tagNames: [val] });
        await loadOrganizeOverview(q.id, true);
      };
      return;
    }

    if (!j) {
      el.innerHTML = `<div class="setup-card" style="text-align:center;padding:24px;"><span class="muted" style="font-size:12px;">Select an item on the left to inspect it.</span></div>`;
      return;
    }

    if (mode === "repair") {
      const rState = snapshot.tab_repair_items?.[j.id];
      const diag = rState?.diagnosis;
      const rep = rState?.report;
      const protoBase = navigator.userAgent.includes("Windows") ? "http://preview.localhost" : "preview://localhost";
      el.innerHTML = `
        <div class="setup-card">
          <span class="setup-kicker">Repair Diagnosis</span>
          <div style="font-size:12.5px;font-weight:600;word-break:break-all;">${escapeHtml(jobFileName(j))}</div>
          ${diag ? `
            <div class="muted tabular-nums" style="font-size:11px;">
              Size: ${fmtBytes(diag.actual_file_size)} · SHA-256: <span class="mono">${escapeHtml((diag.sha256 || "").slice(0, 14))}…</span>
            </div>
            ${diag.detected_issues?.length ? `<ul style="margin:4px 0 0 16px;padding:0;font-size:11px;color:var(--warn);">${diag.detected_issues.map((x) => `<li>${escapeHtml(x)}</li>`).join("")}</ul>` : ""}
            ${diag.atoms?.length ? `<div class="muted mono" style="font-size:10.5px;margin-top:4px;">Atoms: ${diag.atoms.slice(0, 8).map((a) => `${a.box_type}(${fmtBytes(a.size)})`).join(" → ")}</div>` : ""}
          ` : `<p class="muted" style="font-size:11.5px;margin:0;">Click <strong>Diagnose</strong> or <strong>Run Repair</strong> to inspect MP4 atoms and recover streams.</p>`}
        </div>
        ${rState?.preview_sheet_filename ? `
          <div class="setup-card">
            <span class="setup-kicker">Recovered Contact Sheet</span>
            <img src="${protoBase}/${encodeURIComponent(rState.preview_sheet_filename)}" style="width:100%;border-radius:8px;border:1px solid var(--border);" alt="Recovered contact sheet" />
          </div>
        ` : ""}
        ${rep ? `
          <div class="setup-card">
            <span class="setup-kicker">Recovery Report</span>
            <div style="font-size:11.5px;">Strategy: <strong>${escapeHtml(rep.selected_strategy || "—")}</strong></div>
            <div class="muted tabular-nums" style="font-size:11px;">Frames: ${rep.decoded_frames ?? "—"} · Duration: ${fmtDuration(rep.recovered_duration_secs || 0)} · Integrity: ${rep.source_checksum_verified ? "Original untouched ✓" : "—"}</div>
            <div class="inline-row" style="gap:6px;margin-top:4px;">
              <button type="button" id="rep-exp-json" class="btn btn-sm btn-secondary">Export JSON</button>
              <button type="button" id="rep-exp-txt" class="btn btn-sm btn-secondary">Export TXT</button>
            </div>
          </div>
        ` : ""}
      `;
      el.querySelector("#rep-exp-json")?.addEventListener("click", async () => {
        const dest = await dialog.save({ defaultPath: `${jobFileName(j)}.recovery.json` });
        if (dest) invoke("export_tab_repair_report", { jobId: j.id, destPath: dest, format: "json" });
      });
      el.querySelector("#rep-exp-txt")?.addEventListener("click", async () => {
        const dest = await dialog.save({ defaultPath: `${jobFileName(j)}.recovery.txt` });
        if (dest) invoke("export_tab_repair_report", { jobId: j.id, destPath: dest, format: "txt" });
      });
      return;
    }

    // Explore Inspector with REAL 6-Frame Storyboard via get_comparison_frame!
    const dec = jobDecision(j);
    const info = jobVideoInfo(j);
    const stamps = [0.10, 0.25, 0.40, 0.60, 0.75, 0.90];
    const cachedUrls = storyboardCache.get(j.id);

    el.innerHTML = `
      <div class="setup-card">
        <div class="setup-card-head">
          <span class="setup-kicker">6-Frame Scene Preview</span>
          <button type="button" id="ins-preview-15" class="btn-link">15s Compare</button>
        </div>
        <div class="storyboard-grid">
          ${stamps.map((pct, i) => `
            <div class="storyboard-cell">
              ${cachedUrls && cachedUrls[i] ? `<img src="${cachedUrls[i]}" alt="Frame ${Math.round(pct * 100)}%" />` : `<div style="width:100%;height:100%;display:flex;align-items:center;justify-content:center;color:#71717A;font-size:10px;">Loading…</div>`}
              <span class="storyboard-ts">${Math.round(pct * 100)}%</span>
            </div>
          `).join("")}
        </div>
        <div>
          <div style="font-size:12px;font-weight:600;word-break:break-all;">${escapeHtml(jobFileName(j))}</div>
          <div class="muted tabular-nums" style="font-size:11px;margin-top:2px;">${fmtBytes(j.input_bytes)} · ${fmtDuration(info.dur)} · ${escapeHtml(info.codec)} ${info.mbps > 0 ? info.mbps + " Mbps" : ""}</div>
        </div>
      </div>

      <div class="setup-card">
        <span class="setup-kicker">Decision for This Video</span>
        <button type="button" data-ins-dec="protected" class="decision-btn ${dec === "protected" ? "active" : ""}">
          <span>★ Protect &amp; Keep Original</span><span class="muted" style="font-size:10px;">Lock file</span>
        </button>
        <button type="button" data-ins-dec="compress_balanced" class="decision-btn ${dec === "compress_balanced" ? "active" : ""}">
          <span>Compress · Balanced</span><span class="muted" style="font-size:10px;">HEVC CRF 20</span>
        </button>
        <button type="button" data-ins-dec="compress_compact" class="decision-btn ${dec === "compress_compact" ? "active" : ""}">
          <span>Compress · Compact (Max GB)</span><span class="muted" style="font-size:10px;">AV1 CRF 28</span>
        </button>
        <button type="button" data-ins-dec="repair" class="decision-btn ${dec === "repair" ? "active" : ""}">
          <span>Needs Repair</span><span class="muted" style="font-size:10px;">Fix header</span>
        </button>
        <button type="button" id="ins-fwd-one" class="btn btn-sm btn-primary" style="width:100%;margin-top:4px;">Forward This File to Another Tab →</button>
      </div>
    `;

    el.querySelector("#ins-preview-15").onclick = () => openSamplePreviewModal(j);
    el.querySelector("#ins-fwd-one").onclick = () => openForwardModal([j.id], dec === "repair" ? "repair" : "encode", "selected file", false);
    el.querySelectorAll("[data-ins-dec]").forEach((b) => {
      b.onclick = () => {
        const val = b.getAttribute("data-ins-dec");
        invoke("set_job_decision", { jobId: j.id, decision: dec === val ? "unreviewed" : val });
      };
    });

    if (!cachedUrls && !storyboardInflight.has(j.id)) {
      loadRealStoryboardFrames(j, info.dur, stamps);
    }
  }

  async function loadRealStoryboardFrames(job, durSecs, stamps) {
    storyboardInflight.add(job.id);
    const dur = Math.max(2, durSecs || 12);
    try {
      const urls = await Promise.all(stamps.map(async (frac) => {
        try {
          const ts = Math.min(dur - 0.2, Math.max(0.1, dur * frac));
          const bytes = await invoke("get_comparison_frame", { mediaPath: job.source_path, timestampSecs: ts, fullResolution: false });
          return URL.createObjectURL(new Blob([new Uint8Array(bytes)], { type: "image/jpeg" }));
        } catch (_) {
          return "";
        }
      }));
      storyboardCache.set(job.id, urls);
      if (selectedJobId === job.id) renderSidebar();
    } finally {
      storyboardInflight.delete(job.id);
    }
  }

  function renderFooter() {
    const q = activeQueue();
    const dot = $("tool-dot");
    const ts = snapshot.tools;
    if (ts.discovered && ts.capabilities) {
      const codecs = ts.capabilities.available_codecs.map((c) => c.toUpperCase()).join(" + ") || "Remux";
      dot.className = "dot ok";
      $("tool-label").textContent = `FFmpeg ready · ${codecs}`;
    } else {
      dot.className = "dot err";
      $("tool-label").textContent = "Locate FFmpeg";
    }

    const activeJob = snapshot.jobs.find((j) => j.status === "encoding");
    if (snapshot.power_assertion_active || activeJob) {
      $("lbl-power-status").textContent = activeJob
        ? `Encoding ${jobFileName(activeJob)}`
        : "Awake assertion active";
      $("footer-awake-badge").classList.remove("hidden");
    } else {
      $("lbl-power-status").textContent = `Idle · ${snapshot.queues.length} workspace tab${snapshot.queues.length === 1 ? "" : "s"}`;
      $("footer-awake-badge").classList.add("hidden");
    }
    $("lbl-scheduler-status").textContent = snapshot.scheduler_busy_reason || "";
  }

  // =========================================================================
  // 6. FORWARD MODAL & EXISTING FEATURE MODALS
  // =========================================================================
  function getDefaultForwardPlan(q, jobs) {
    const mode = activeMode(q);
    if (mode === "explore") {
      const comp = jobs.filter((j) => ["compress_balanced", "compress_compact"].includes(jobDecision(j)));
      if (comp.length > 0) return { jobIds: comp.map((j) => j.id), targetMode: "encode", label: "clips tagged for compression", useOutputFile: false };
      const rep = jobs.filter((j) => jobDecision(j) === "repair");
      if (rep.length > 0) return { jobIds: rep.map((j) => j.id), targetMode: "repair", label: "clips tagged for repair", useOutputFile: false };
      const sel = jobs.find((j) => j.id === selectedJobId);
      const nonProt = jobs.filter((j) => jobDecision(j) !== "protected");
      const pick = sel ? [sel] : nonProt;
      return { jobIds: pick.map((j) => j.id), targetMode: "encode", label: "selected clips", useOutputFile: false };
    }
    if (mode === "repair") {
      const repDone = jobs.filter((j) => {
        const st = snapshot.tab_repair_items?.[j.id]?.status;
        return ["recovered", "partial_recovery"].includes(st);
      });
      if (repDone.length > 0) return { jobIds: repDone.map((j) => j.id), targetMode: "encode", label: "verified repaired outputs", useOutputFile: true };
      return { jobIds: jobs.map((j) => j.id), targetMode: "encode", label: "clips", useOutputFile: false };
    }
    if (mode === "organize") {
      const s = getOrgSelectedSet(q.id);
      if (s.size > 0) {
        const kept = jobs.filter((j) => !s.has(j.id));
        if (kept.length > 0) return { jobIds: kept.map((k) => k.id), targetMode: "encode", label: "organized items", useOutputFile: false };
      }
      return { jobIds: jobs.map((j) => j.id), targetMode: "encode", label: "collection items", useOutputFile: false };
    }
    // encode mode
    const done = jobs.filter((j) => j.status === "completed");
    if (done.length > 0) return { jobIds: done.map((j) => j.id), targetMode: "explore", label: "completed outputs", useOutputFile: true };
    const review = jobs.filter((j) => j.status === "needs_attention");
    if (review.length > 0) return { jobIds: review.map((j) => j.id), targetMode: "repair", label: "clips needing repair", useOutputFile: false };
    return { jobIds: jobs.map((j) => j.id), targetMode: "explore", label: "queue clips", useOutputFile: false };
  }

  function openForwardModal(customJobIds = null, targetMode = null, customLabel = null, useOutputFile = null) {
    const q = activeQueue();
    if (!q) return;
    const jobs = activeQueueJobs();
    const def = getDefaultForwardPlan(q, jobs);
    const jobIds = customJobIds || def.jobIds;
    const destMode = targetMode || def.targetMode;
    const label = customLabel || def.label;
    const useOut = useOutputFile !== null ? useOutputFile : def.useOutputFile;
    if (!jobIds || jobIds.length === 0) {
      return showToast("Select, tag, or finish at least one file to forward it.", "warn");
    }

    const chosenJobs = jobIds.map((id) => snapshot.jobs.find((j) => j.id === id)).filter(Boolean);
    const otherTabs = snapshot.queues.filter((oq) => oq.id !== q.id);
    const preferredExisting = otherTabs.find((oq) => activeMode(oq) === destMode);
    const defaultNewName = destMode === "encode" ? `${q.name} · Compress` : destMode === "repair" ? `${q.name} · Repair` : destMode === "organize" ? `${q.name} · Organize` : `${q.name} · Explore`;

    const wrap = document.createElement("div");
    wrap.className = "sidebar-panel-stack";
    wrap.innerHTML = `
      <div class="muted" style="font-size:12px;">Forwarding <strong>${chosenJobs.length}</strong> ${escapeHtml(label)} from "${escapeHtml(q.name)}"</div>
      <div style="max-height:150px;overflow-y:auto;padding:8px;border-radius:10px;background:var(--surface-2);border:1px solid var(--border);display:flex;flex-direction:column;gap:5px;">
        ${chosenJobs.map((j) => `
          <div class="inline-row" style="justify-content:space-between;font-size:11.5px;">
            <span class="truncate" style="font-weight:500;">${escapeHtml(useOut && snapshot.tab_repair_items?.[j.id]?.output_path ? snapshot.tab_repair_items[j.id].output_path.split(/[\\/]/).pop() : jobFileName(j))}</span>
            <span class="muted tabular-nums">${fmtBytes(j.input_bytes)}</span>
          </div>
        `).join("")}
      </div>
      <div class="field-block">
        <label class="field-label" for="fwd-target-sel">Destination Tab</label>
        <select id="fwd-target-sel" class="full-input">
          ${otherTabs.map((ot) => `<option value="${escapeHtml(ot.id)}" ${preferredExisting?.id === ot.id ? "selected" : ""}>Existing: ${escapeHtml(ot.name)} (${(MODE_META[activeMode(ot)] || MODE_META.encode).label})</option>`).join("")}
          <option value="__new__" ${!preferredExisting ? "selected" : ""}>+ Create New ${(MODE_META[destMode] || MODE_META.encode).fullLabel} Tab…</option>
        </select>
      </div>
      <div id="fwd-new-row" class="${preferredExisting ? "hidden" : ""} seg-pair-grid">
        <div class="field-block">
          <label class="field-label" for="fwd-new-name">New Tab Name</label>
          <input id="fwd-new-name" class="full-input" type="text" value="${escapeHtml(defaultNewName)}" />
        </div>
        <div class="field-block">
          <label class="field-label" for="fwd-new-mode">Action Mode</label>
          <select id="fwd-new-mode" class="full-input">
            <option value="encode" ${destMode === "encode" ? "selected" : ""}>Transcode Queue</option>
            <option value="repair" ${destMode === "repair" ? "selected" : ""}>Repair Videos</option>
            <option value="organize" ${destMode === "organize" ? "selected" : ""}>Organize Collection</option>
            <option value="explore" ${destMode === "explore" ? "selected" : ""}>Explore &amp; Triage</option>
          </select>
        </div>
      </div>
    `;

    const sel = wrap.querySelector("#fwd-target-sel");
    const newRow = wrap.querySelector("#fwd-new-row");
    sel.onchange = () => newRow.classList.toggle("hidden", sel.value !== "__new__");

    openModalEl("Forward Resources to Tab", wrap, [
      { label: "Cancel", cls: "btn", onClick: closeModal },
      {
        label: "Forward & Open Tab",
        cls: "btn btn-primary",
        onClick: async () => {
          const isNew = sel.value === "__new__";
          try {
            const dest = await invoke("forward_resources_to_tab", {
              fromQueueId: q.id,
              targetQueueId: isNew ? null : sel.value,
              newTabName: isNew ? wrap.querySelector("#fwd-new-name").value : null,
              newTabMode: isNew ? wrap.querySelector("#fwd-new-mode").value : null,
              jobIds,
              useOutputFile: Boolean(useOut),
            });
            closeModal();
            showToast(`Forwarded ${chosenJobs.length} item${chosenJobs.length === 1 ? "" : "s"} to "${dest.name}"`, "success");
          } catch (err) {
            showToast(String(err), "warn");
          }
        },
      },
    ]);
  }

  function openIconShowcaseModal() {
    const wrap = document.createElement("div");
    wrap.className = "sidebar-panel-stack";
    wrap.innerHTML = `
      <div class="seg-pair-grid">
        <div style="border-radius:14px;background:#121212;border:1px solid rgba(255,255,255,0.1);padding:20px;display:flex;flex-direction:column;align-items:center;gap:10px;">
          <div style="width:96px;height:96px;border-radius:22px;overflow:hidden;box-shadow:0 12px 28px rgba(0,0,0,0.5);">${APP_ICON_SVG}</div>
          <span style="font-size:11px;color:#A1A1AA;">Dark Dock</span>
        </div>
        <div style="border-radius:14px;background:#F4F4F2;border:1px solid #E4E4E2;padding:20px;display:flex;flex-direction:column;align-items:center;gap:10px;">
          <div style="width:96px;height:96px;border-radius:22px;overflow:hidden;box-shadow:0 12px 28px rgba(0,0,0,0.14);">${APP_ICON_SVG}</div>
          <span style="font-size:11px;color:#52525B;">Light Desktop</span>
        </div>
      </div>
      <div class="setup-row-between" style="padding:10px;border-radius:10px;background:var(--surface-2);border:1px solid var(--border);">
        <span class="muted" style="font-size:12px;">Media Shack vector app icon (SVG)</span>
        <button type="button" id="ic-dl-btn" class="btn btn-sm btn-primary">Save SVG…</button>
      </div>
    `;
    wrap.querySelector("#ic-dl-btn").onclick = async () => {
      const blob = new Blob([APP_ICON_SVG], { type: "image/svg+xml;charset=utf-8" });
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = "media-shack-icon.svg";
      document.body.appendChild(a);
      a.click();
      a.remove();
      URL.revokeObjectURL(url);
      showToast("Saved media-shack-icon.svg", "success");
    };
    openModalEl("Media Shack — App Icon", wrap, [{ label: "Done", cls: "btn btn-primary", onClick: closeModal }]);
  }

  // Import & Staging Flow
  let latestImportProgress = null;
  function updateScanProgressDom() {
    const wrap = $("import-scan-progress-box");
    if (!wrap || !latestImportProgress) return;
    const p = latestImportProgress;
    const titleEl = wrap.querySelector("#scan-prog-title");
    const subEl = wrap.querySelector("#scan-prog-sub");
    const fillEl = wrap.querySelector("#scan-prog-fill");
    const pct = p.total > 0 ? Math.min(100, Math.round((p.probed / p.total) * 100)) : 28;
    if (p.phase === "discovering") {
      if (titleEl) titleEl.textContent = `Discovering files… Found ${p.discovered} item${p.discovered === 1 ? "" : "s"}`;
      if (fillEl) fillEl.style.width = `${Math.min(45, 10 + Math.min(35, p.discovered))}%`;
    } else if (p.phase === "loading") {
      if (titleEl) titleEl.textContent = `Loading ${p.probed} / ${p.total} discovered files (stats run asynchronously)…`;
      if (fillEl) fillEl.style.width = `${pct}%`;
    } else {
      if (titleEl) titleEl.textContent = `Probing stats ${p.probed} / ${p.total} (${p.discovered} discovered)…`;
      if (fillEl) fillEl.style.width = `${pct}%`;
    }
    if (subEl) subEl.textContent = p.current_file || "Scanning directories…";
  }

  async function startImportFlow(paths) {
    const q = activeQueue();
    if (!q || !paths || paths.length === 0) return;
    const mode = activeMode(q);
    latestImportProgress = { phase: "discovering", discovered: 0, probed: 0, total: 0, current_file: "Scanning folders…" };
    const scanBox = document.createElement("div");
    scanBox.id = "import-scan-progress-box";
    scanBox.className = "sidebar-panel-stack";
    scanBox.innerHTML = `
      <div id="scan-prog-title" style="font-size:13px;font-weight:600;">Discovering media for "${escapeHtml(q.name)}"…</div>
      <div class="progress-track"><div id="scan-prog-fill" class="progress-fill" style="width:12%;"></div></div>
      <div id="scan-prog-sub" class="mono muted truncate" style="font-size:11px;">Starting scan…</div>
    `;
    openModalEl("Scanning media…", scanBox, [
      { label: "Cancel scan", cls: "btn", onClick: async () => { await invoke("cancel_import_scan"); closeModal(); } },
    ]);
    try {
      const scan = await invoke("start_import_scan", {
        queueId: q.id,
        paths,
        recursive: q.recursive_import,
        destinationRootOverride: q.destination_root,
        deferProbe: true,
      });
      if (scan.canceled) return closeModal();

      // In Repair mode, include even unreadable/damaged containers so user can diagnose/recover them
      if (mode === "repair") {
        scan.items.forEach((it) => { if (!it.duplicate_in_queue) it.include = true; });
      }
      stagingState = scan;

      // Unified Table / Workspace Flow: always load discovered items directly & compute stats async
      const chosen = scan.items.filter((i) => i.include).map((i) => ({
        source_path: i.source_path,
        source_rel_path: i.source_rel_path,
        source_root: i.source_root,
        destination_path: i.destination_path,
        input_bytes: i.input_bytes,
        probe: i.probe,
        plan: i.plan,
        unreadable_error: i.unreadable_error,
      }));
      if (chosen.length > 0) {
        await invoke("commit_staged_import", {
          queueId: scan.queue_id,
          destinationRoot: scan.destination_root,
          recipe: scan.recipe,
          items: chosen,
        });
        closeModal();
        showToast(
          mode === "encode"
            ? `Loaded ${chosen.length} file${chosen.length === 1 ? "" : "s"} into table — check rows or apply criteria to queue.`
            : `Loaded ${chosen.length} file${chosen.length === 1 ? "" : "s"} into "${q.name}" — computing media stats in background.`,
          "success"
        );
        return;
      }
      closeModal();
      showToast(`No new files found to attach (${scan.skipped_count} skipped).`, "info");
    } catch (err) {
      openModal("Import Error", `<p>${escapeHtml(String(err))}</p>`, [{ label: "Close", cls: "btn btn-primary", onClick: closeModal }]);
    }
  }

  function renderStagingModal() {
    const s = stagingState;
    if (!s) return;
    const isRepairMode = activeMode(activeQueue()) === "repair";
    let sortBy = "name";
    const draw = () => {
      const sorted = [...s.items].sort((a, b) => sortBy === "size" ? b.input_bytes - a.input_bytes : sortBy === "bitrate" ? (b.bit_rate || 0) - (a.bit_rate || 0) : a.source_rel_path.localeCompare(b.source_rel_path));
      const includedCount = s.items.filter((i) => i.include).length;
      const body = document.createElement("div");
      body.innerHTML = `
        <div class="setup-row-between" style="flex-wrap:wrap;margin-bottom:10px;">
          <div>
            <strong>Tab:</strong> ${escapeHtml(s.queue_name)}
            <div class="muted" style="font-size:12px;margin-top:2px;">Dest: <span class="mono">${escapeHtml(s.destination_root)}</span> <button id="stg-dest-btn" class="btn-link" type="button">Change…</button></div>
          </div>
          <label class="inline-row" style="font-size:12px;"><span>Sort:</span>
            <select id="stg-sort" class="compact-sel">
              <option value="name" ${sortBy === "name" ? "selected" : ""}>Name</option>
              <option value="size" ${sortBy === "size" ? "selected" : ""}>Largest</option>
              <option value="bitrate" ${sortBy === "bitrate" ? "selected" : ""}>Bitrate</option>
            </select>
          </label>
        </div>
        <table class="table-simple">
          <thead><tr><th>Add</th><th>File</th><th>Codec / Bitrate</th><th>Size</th><th>Status</th></tr></thead>
          <tbody id="stg-tbody"></tbody>
        </table>`;
      const tbody = body.querySelector("#stg-tbody");
      for (const it of sorted) {
        const tr = document.createElement("tr");
        const stNotes = [];
        if (it.duplicate_in_queue) stNotes.push("Already in tab");
        if (it.unreadable_error) stNotes.push(it.unreadable_error);
        else if (it.plan && !it.plan.is_ready) stNotes.push("Needs review");
        const disableChk = it.unreadable_error && !isRepairMode;
        tr.innerHTML = `<td><input type="checkbox" ${it.include ? "checked" : ""} ${disableChk ? "disabled" : ""}/></td>
          <td><div class="truncate" style="max-width:250px;">${escapeHtml(it.source_rel_path)}</div></td>
          <td>${escapeHtml(it.current_codec || "—")} · ${it.bit_rate ? Math.round(it.bit_rate / 1000) + " kbps" : "—"}</td>
          <td class="tabular-nums">${fmtBytes(it.input_bytes)}</td>
          <td>${escapeHtml(stNotes.join("; ") || "Ready")}</td>`;
        tr.querySelector("input").addEventListener("change", (e) => { it.include = e.target.checked; draw(); });
        tbody.appendChild(tr);
      }
      body.querySelector("#stg-dest-btn").onclick = async () => {
        const dir = await dialog.open({ directory: true, defaultPath: s.destination_root });
        if (dir) { s.destination_root = dir; draw(); }
      };
      body.querySelector("#stg-sort").onchange = (e) => { sortBy = e.target.value; draw(); };
      openModalEl(`Attach Resources (${includedCount} of ${s.items.length})`, body, [
        { label: "Cancel", cls: "btn", onClick: closeModal },
        {
          label: `Add ${includedCount} files to ${s.queue_name}`,
          cls: "btn btn-primary",
          disabled: includedCount === 0,
          onClick: async () => {
            const chosen = s.items.filter((i) => i.include).map((i) => ({
              source_path: i.source_path, source_rel_path: i.source_rel_path, source_root: i.source_root,
              destination_path: i.destination_path, input_bytes: i.input_bytes, probe: i.probe, plan: i.plan, unreadable_error: i.unreadable_error,
            }));
            await invoke("commit_staged_import", { queueId: s.queue_id, destinationRoot: s.destination_root, recipe: s.recipe, items: chosen });
            closeModal();
          },
        },
      ]);
    };
    draw();
  }

  // Settings & Recipe Helpers
  function checkRecipeCustom(r) {
    if (r.family === "remux") return false;
    const defaults = { "best:hevc": [17, "slower"], "balanced:hevc": [20, "slow"], "best:av1": [22, "3"], "balanced:av1": [27, "5"] }[`${r.variant}:${r.codec}`];
    const hasCustomTuning = Boolean(r.tune) || (r.ten_bit === false) || Boolean(r.aq_mode);
    return (defaults ? (Number(r.crf) !== defaults[0] || String(r.effort) !== defaults[1]) : false) || hasCustomTuning;
  }

  function applyCodecDefaults(r) {
    if (r.family === "remux") {
      r.crf = 0; r.effort = "none"; r.tune = null; r.ten_bit = false; r.aq_mode = null; r.is_custom = false;
      return;
    }
    const defaults = { "best:hevc": [17, "slower"], "balanced:hevc": [20, "slow"], "best:av1": [22, "3"], "balanced:av1": [27, "5"] }[`${r.variant}:${r.codec}`];
    if (defaults) { r.crf = defaults[0]; r.effort = defaults[1]; }
    r.ten_bit = true;
    if (r.codec === "av1") { r.tune = null; r.aq_mode = null; }
    r.is_custom = checkRecipeCustom(r);
  }

  async function patchActiveQueueSettings(mutator) {
    const q = activeQueue();
    if (!q) return;
    const patch = {
      recipe: { ...q.recipe }, priority: q.priority, recursive_import: q.recursive_import, destination_root: q.destination_root,
      keep_awake: q.keep_awake, error_policy: q.error_policy, min_savings_enabled: q.min_savings_enabled, min_savings_pct: q.min_savings_pct,
      cleanup_policy: q.cleanup_policy, full_decode_verify: q.full_decode_verify,
    };
    mutator(patch);
    patch.recipe.is_custom = checkRecipeCustom(patch.recipe);
    await invoke("update_queue_settings", { queueId: q.id, patch });
  }

  function statusLabel(st) {
    return { listed: "Ready", queued: "Queued", probing: "Probing", ready: "In Queue", needs_attention: "Needs review", encoding: "Encoding", verifying: "Verifying", publishing: "Publishing", completed: "Verified", below_savings_target: "Kept original (<savings)", failed: "Failed", canceled: "Canceled", interrupted: "Interrupted" }[st] || st;
  }
  function statusBadgeClass(st) {
    if (st === "completed") return "badge-ok";
    if (["encoding", "verifying", "publishing"].includes(st)) return "badge-active";
    if (st === "failed") return "badge-err";
    if (["needs_attention", "interrupted", "below_savings_target"].includes(st)) return "badge-warn";
    return "";
  }
  function cleanupOutcomeLabel(co) {
    return { none: "Original kept", kept: "Original kept", pending: "Pending review", trashed: "Moved to Trash", skipped: "Cleanup skipped", declined: "Kept by user", uncertain: "Review cleanup", failed: "Cleanup failed" }[co] || co;
  }

  // Shared Modal Utilities + Preview / Review / Live / Tools / Profiles
  function openModal(title, html, buttons) {
    const div = document.createElement("div");
    div.innerHTML = html;
    openModalEl(title, div, buttons);
  }
  function openModalEl(title, bodyEl, buttons) {
    const root = $("modal-root");
    const box = $("modal-box");
    box.innerHTML = "";
    const head = document.createElement("div");
    head.className = "modal-head";
    head.innerHTML = `<h2>${escapeHtml(title)}</h2>`;
    const xBtn = document.createElement("button");
    xBtn.type = "button"; xBtn.className = "btn btn-ghost btn-sm"; xBtn.textContent = "×"; xBtn.onclick = closeModal;
    head.appendChild(xBtn);
    const body = document.createElement("div");
    body.className = "modal-body";
    body.appendChild(bodyEl);
    const foot = document.createElement("div");
    foot.className = "modal-foot";
    for (const b of buttons) {
      const btn = document.createElement("button");
      btn.type = "button"; btn.className = b.cls || "btn"; btn.textContent = b.label; btn.disabled = !!b.disabled;
      if (b.autofocus) btn.autofocus = true;
      btn.onclick = b.onClick;
      foot.appendChild(btn);
    }
    box.append(head, body, foot);
    root.classList.remove("hidden");
  }
  function closeModal() {
    $("modal-root").classList.add("hidden");
    openSampleRefresh = null;
    if (livePreviewTimer) { clearInterval(livePreviewTimer); livePreviewTimer = null; }
    if (currentLiveJobId) { invoke("stop_live_preview").catch(() => {}); currentLiveJobId = null; }
  }
  function openPromptModal(title, label, defaultValue, onConfirm) {
    const wrap = document.createElement("div");
    wrap.innerHTML = `<div class="field"><label for="prm-input">${escapeHtml(label)}</label><input id="prm-input" class="full-input" type="text" value="${escapeHtml(defaultValue || "")}" /></div>`;
    const inputEl = wrap.querySelector("#prm-input");
    const submit = async () => { const v = inputEl.value.trim(); if (!v) return; closeModal(); await onConfirm(v); };
    inputEl.addEventListener("keydown", (e) => { if (e.key === "Enter") { e.preventDefault(); submit(); } });
    openModalEl(title, wrap, [{ label: "Cancel", cls: "btn", onClick: closeModal }, { label: "Save", cls: "btn btn-primary", onClick: submit }]);
    setTimeout(() => { inputEl.focus(); inputEl.select(); }, 20);
  }

  function openToolsModal() {
    const ts = snapshot.tools;
    const caps = ts.capabilities;
    let html = caps
      ? `<p><strong>ffmpeg:</strong> <span class="mono">${escapeHtml(caps.ffmpeg_path)}</span><br><span class="muted">${escapeHtml(caps.ffmpeg_version)}</span></p>
         <p><strong>ffprobe:</strong> <span class="mono">${escapeHtml(caps.ffprobe_path)}</span><br><span class="muted">${escapeHtml(caps.ffprobe_version)}</span></p>
         <p><strong>Available encoders:</strong> ${caps.available_codecs.map((c) => c.toUpperCase()).join(", ") || "None (Remux only)"}</p>`
      : `<p><strong>Status:</strong> ${escapeHtml(ts.error || "Not found")}</p><p>${escapeHtml(ts.repair_hint || "")}</p>`;
    openModal("FFmpeg & ffprobe Tools", html, [
      { label: "Select tools folder…", cls: "btn", onClick: async () => { const dir = await dialog.open({ directory: true }); if (dir) { await invoke("locate_ffmpeg_tools", { toolsDir: dir }); openToolsModal(); } } },
      { label: "Recheck", cls: "btn", onClick: async () => { await invoke("recheck_tools"); openToolsModal(); } },
      { label: "Done", cls: "btn btn-primary", onClick: closeModal },
    ]);
  }

  function openProfilesModal() {
    const q = activeQueue();
    if (!q) return;
    const wrap = document.createElement("div");
    wrap.className = "sidebar-panel-stack";
    wrap.innerHTML = `<p class="muted" style="margin:0;font-size:12px;">Save or manage reusable batch workflow profiles.</p>`;
    for (const p of snapshot.profiles) {
      const row = document.createElement("div");
      row.className = "setup-row-between";
      row.style.cssText = "padding:8px 10px;border:1px solid var(--border);border-radius:8px;font-size:12px;";
      row.innerHTML = `<span><strong>${escapeHtml(p.name)}</strong> <span class="muted">(${p.recipe.family === "remux" ? "Remux" : `${p.recipe.codec.toUpperCase()} CRF ${p.recipe.crf}`})</span></span>
        <div class="inline-row" style="gap:4px;"><button type="button" class="btn btn-sm" data-upd>Update</button><button type="button" class="btn btn-sm btn-danger-outline" data-del>Delete</button></div>`;
      row.querySelector("[data-upd]").onclick = async () => { await invoke("save_profile_from_queue", { queueId: q.id, name: p.name, existingProfileId: p.id }); openProfilesModal(); };
      row.querySelector("[data-del]").onclick = async () => { await invoke("delete_profile", { profileId: p.id }); await refreshSnapshot(); openProfilesModal(); };
      wrap.appendChild(row);
    }
    openModalEl("Manage Workflow Profiles", wrap, [
      { label: "Save current as new profile…", cls: "btn btn-primary", onClick: () => openPromptModal("Save Workflow Profile", "Profile name:", "Overnight archive", async (nm) => { await invoke("save_profile_from_queue", { queueId: q.id, name: nm, existingProfileId: null }); await refreshSnapshot(); openProfilesModal(); }) },
      { label: "Close", cls: "btn", onClick: closeModal },
    ]);
  }

  function openCleanupConsentModal(q, resp, selectedJobIds = null) {
    openModal(
      "Confirm Batch Source Cleanup",
      `<p><strong>Automatic Trash is enabled for this queue.</strong> Move ${resp.consent_jobs.length} verified originals (${fmtBytes(resp.consent_total_bytes)}) to OS Trash after full decode verification?</p>`,
      [
        { label: "Keep originals (recommended)", cls: "btn btn-primary", onClick: async () => { await patchActiveQueueSettings((p) => { p.cleanup_policy = "keep_originals"; }); closeModal(); await invoke("start_queue", { queueId: q.id, consentConfirmed: false, selectedJobIds }); } },
        { label: `Trash ${resp.consent_jobs.length} verified originals`, cls: "btn btn-danger", onClick: async () => { closeModal(); await invoke("start_queue", { queueId: q.id, consentConfirmed: true, selectedJobIds }); } },
      ]
    );
  }

  async function openReviewOriginalsModal(queueId) {
    const candidates = await invoke("get_cleanup_candidates", { queueId });
    const selectedIds = new Set();
    const draw = () => {
      const wrap = document.createElement("div");
      const totalSelectedBytes = candidates.filter((c) => selectedIds.has(c.job_id)).reduce((acc, c) => acc + c.input_bytes, 0);
      wrap.innerHTML = `<p style="font-size:12px;">Move verified originals to recoverable OS Trash. Nothing is selected by default.</p>`;
      const tbl = document.createElement("table");
      tbl.className = "table-simple";
      tbl.innerHTML = `<thead><tr><th>Trash</th><th>Source</th><th>Original → Output</th><th>Status</th></tr></thead><tbody></tbody>`;
      const tbody = tbl.querySelector("tbody");
      for (const c of candidates) {
        const prot = isProtectedSource(c.source_path);
        const canSelect = c.eligible && c.cleanup_outcome !== "trashed" && !prot;
        const tr = document.createElement("tr");
        tr.innerHTML = `<td><input type="checkbox" ${selectedIds.has(c.job_id) ? "checked" : ""} ${canSelect ? "" : "disabled"}/></td>
          <td class="mono truncate" style="max-width:210px;">${escapeHtml(c.source_path)}</td>
          <td class="tabular-nums">${fmtBytes(c.input_bytes)} → ${fmtBytes(c.output_bytes)}</td>
          <td>${prot ? "Protected ★ Locked" : escapeHtml(c.cleanup_outcome === "trashed" ? "Moved to Trash" : c.eligible ? c.codec_label : (c.ineligibility_reason || "Ineligible"))}</td>`;
        tr.querySelector("input").addEventListener("change", (e) => { if (e.target.checked) selectedIds.add(c.job_id); else selectedIds.delete(c.job_id); draw(); });
        tbody.appendChild(tr);
      }
      wrap.appendChild(tbl);
      openModalEl("Review Originals", wrap, [
        { label: "Keep remaining & Close", cls: "btn btn-primary", onClick: closeModal },
        {
          label: `Move ${selectedIds.size} selected (${fmtBytes(totalSelectedBytes)}) to Trash`,
          cls: "btn btn-danger",
          disabled: selectedIds.size === 0,
          onClick: async () => {
            await invoke("trash_selected_originals", { jobIds: Array.from(selectedIds) });
            await refreshSnapshot();
            openReviewOriginalsModal(queueId);
          },
        },
      ]);
    };
    draw();
  }

  async function openSamplePreviewModal(job) {
    const dur = job.probe?.format?.duration_secs || 60;
    const defaultStart = dur > 15 ? Math.min(dur * 0.25, Math.max(0, dur - 15)) : 0;
    let startInput = defaultStart;
    let seekOffset = 0;
    let zoom100 = false;
    let usingFrameFallback = false;

    const draw = async () => {
      const sp = snapshot.sample_preview && snapshot.sample_preview.job_id === job.id ? snapshot.sample_preview : null;
      const wrap = document.createElement("div");
      wrap.innerHTML = `
        <div class="inline-row" style="gap:8px;margin-bottom:10px;flex-wrap:wrap;">
          <label>Start (s):</label>
          <input id="smp-start" type="number" min="0" max="${Math.max(0, dur - 1).toFixed(1)}" step="1" value="${startInput.toFixed(1)}" style="width:75px" />
          <input id="smp-scrub" type="range" min="0" max="${Math.max(0, dur - 1).toFixed(1)}" step="0.5" value="${startInput.toFixed(1)}" style="flex:1" />
          <button id="smp-run" class="btn btn-primary btn-sm" type="button">Encode 15s sample</button>
        </div>`;
      if (sp) {
        if (sp.status === "queued" || sp.status === "encoding") {
          wrap.innerHTML += `<p>${escapeHtml(sp.waiting_reason || "Encoding 15-second sample…")}</p>`;
        } else if (sp.status === "failed") {
          wrap.innerHTML += `<div class="banner-card warn">${escapeHtml(sp.error || "Sample failed")}</div>`;
        } else if (sp.status === "ready") {
          const proj = sp.projected_bytes ? ` · Projected full file: ${fmtBytes(sp.projected_bytes)} (~${fmtDuration(sp.projected_secs)})` : "";
          wrap.innerHTML += `<p><strong>Sample ready:</strong> ${fmtBytes(sp.measured_bytes)} in ${sp.elapsed_secs?.toFixed(1)}s${proj}</p>
            <div class="inline-row" style="gap:8px;margin-bottom:8px;flex-wrap:wrap;">
              <button id="cmp-play" class="btn btn-sm" type="button">Play / Pause</button>
              <input id="cmp-seek" type="range" min="0" max="${sp.duration_secs.toFixed(1)}" step="0.2" value="${seekOffset}" style="flex:1;min-width:120px;" />
              <button id="cmp-zoom" class="btn btn-sm" type="button">${zoom100 ? "Fit" : "100%"}</button>
              <button id="cmp-frames" class="btn btn-sm" type="button">Exact frames</button>
              <label class="inline-row" style="gap:4px;font-size:12px;cursor:pointer;"><input type="checkbox" id="cmp-delta-chk" /> Show Delta</label>
              <select id="cmp-delta-gain" class="hidden" style="width:74px;font-size:11px;"><option value="4" selected>4× diff</option><option value="8">8× diff</option><option value="16">16× diff</option></select>
              <span id="cmp-delta-stats" class="muted tabular-nums" style="font-size:11px;"></span>
            </div>
            <div id="cmp-grid" class="compare-grid">
              <div class="compare-pane ${zoom100 ? "zoom-100" : ""}"><div class="compare-label">Reference (Original)</div><video id="vid-orig" controls muted playsinline></video><img id="img-orig" class="hidden" /></div>
              <div class="compare-pane ${zoom100 ? "zoom-100" : ""}"><div class="compare-label">Test (Encoded · ${escapeHtml(sp.recipe_snapshot.codec.toUpperCase())} CRF ${sp.recipe_snapshot.crf})</div><video id="vid-enc" controls muted playsinline></video><img id="img-enc" class="hidden" /></div>
              <div id="pane-cmp-delta" class="compare-pane ${zoom100 ? "zoom-100" : ""} hidden"><div class="compare-label">Delta (Amplified Diff)</div><img id="img-cmp-delta" /></div>
            </div>`;
        }
      }
      openModalEl(`15-Second Sample — ${jobFileName(job)}`, wrap, [{ label: "Done", cls: "btn btn-primary", onClick: closeModal }]);
      wrap.querySelector("#smp-start").onchange = (e) => { startInput = Number(e.target.value); wrap.querySelector("#smp-scrub").value = startInput; };
      wrap.querySelector("#smp-scrub").oninput = (e) => { startInput = Number(e.target.value); wrap.querySelector("#smp-start").value = startInput; };
      wrap.querySelector("#smp-run").onclick = async () => { await invoke("request_sample_preview", { jobId: job.id, startSecs: startInput }); await refreshSnapshot(); draw(); };

      if (sp && sp.status === "ready") {
        const vo = wrap.querySelector("#vid-orig"), ve = wrap.querySelector("#vid-enc"), io = wrap.querySelector("#img-orig"), ie = wrap.querySelector("#img-enc");
        const deltaPane = wrap.querySelector("#pane-cmp-delta"), deltaImg = wrap.querySelector("#img-cmp-delta"), deltaChk = wrap.querySelector("#cmp-delta-chk"), deltaGain = wrap.querySelector("#cmp-delta-gain"), deltaStats = wrap.querySelector("#cmp-delta-stats"), cmpGrid = wrap.querySelector("#cmp-grid");
        const protoBase = navigator.userAgent.includes("Windows") ? "http://preview.localhost" : "preview://localhost";
        vo.src = `${protoBase}/${encodeURIComponent(`${job.id}/original_sample.mp4`)}`;
        ve.src = `${protoBase}/${encodeURIComponent(`${job.id}/encoded_sample.${sp.recipe_snapshot.container}`)}`;
        const loadExactFrames = async (t) => {
          usingFrameFallback = true;
          const showDelta = Boolean(deltaChk?.checked);
          const gain = Number(deltaGain?.value || 4);
          const pair = await invoke("get_sample_comparison_pair", {
            sourcePath: job.source_path,
            encodedPath: sp.encoded_sample_path,
            origTs: sp.start_secs + t,
            encTs: t,
            fullResolution: zoom100,
            includeDelta: showDelta,
            deltaGain: gain,
          });
          io.src = pair.orig_data_url;
          ie.src = pair.enc_data_url;
          vo.classList.add("hidden"); ve.classList.add("hidden"); io.classList.remove("hidden"); ie.classList.remove("hidden");
          if (showDelta && pair.delta_data_url) {
            deltaImg.src = pair.delta_data_url;
            deltaPane.classList.remove("hidden");
            cmpGrid.classList.add("cols-3");
            if (deltaStats) deltaStats.textContent = `Diff MAE ${pair.mae_pct?.toFixed(2) ?? "—"}% · PSNR ${pair.psnr_db?.toFixed(1) ?? "—"} dB`;
          } else {
            deltaPane.classList.add("hidden");
            cmpGrid.classList.remove("cols-3");
            if (deltaStats) deltaStats.textContent = "";
          }
        };
        deltaChk?.addEventListener("change", () => {
          deltaGain?.classList.toggle("hidden", !deltaChk.checked);
          if (deltaChk.checked) {
            vo.pause(); ve.pause();
            loadExactFrames(seekOffset).catch(() => {});
          } else {
            deltaPane.classList.add("hidden");
            cmpGrid.classList.remove("cols-3");
            if (deltaStats) deltaStats.textContent = "";
          }
        });
        deltaGain?.addEventListener("change", () => { if (deltaChk?.checked) loadExactFrames(seekOffset).catch(() => {}); });
        ve.onerror = () => loadExactFrames(seekOffset).catch(() => {});
        vo.onerror = () => loadExactFrames(seekOffset).catch(() => {});
        wrap.querySelector("#cmp-play").onclick = () => { if (vo.paused) { vo.play().catch(() => {}); ve.play().catch(() => {}); } else { vo.pause(); ve.pause(); } };
        wrap.querySelector("#cmp-seek").oninput = (e) => { seekOffset = Number(e.target.value); vo.currentTime = seekOffset; ve.currentTime = seekOffset; if (usingFrameFallback || deltaChk?.checked) loadExactFrames(seekOffset).catch(() => {}); };
        wrap.querySelector("#cmp-zoom").onclick = () => { zoom100 = !zoom100; usingFrameFallback = zoom100; draw(); };
        wrap.querySelector("#cmp-frames").onclick = () => loadExactFrames(seekOffset);
      }
    };
    openSampleRefresh = draw;
    draw();
  }

  async function openLivePreviewModal(jobId) {
    currentLiveJobId = jobId;
    await invoke("start_live_preview", { jobId });
    let showSideBySide = true;
    let showDelta = false;
    let deltaGain = 4;

    const wrap = document.createElement("div");
    wrap.innerHTML = `
      <div class="inline-row" style="justify-content:space-between;gap:8px;margin-bottom:8px;flex-wrap:wrap;">
        <div id="lp-meta" class="tabular-nums" style="font-size:12px;">Waiting for first decodable frame…</div>
        <div class="inline-row" style="gap:10px;flex-wrap:wrap;">
          <label class="inline-row" style="gap:4px;font-size:12px;cursor:pointer;"><input type="checkbox" id="lp-sbs-chk" checked /> Ref | Test side-by-side</label>
          <label class="inline-row" style="gap:4px;font-size:12px;cursor:pointer;"><input type="checkbox" id="lp-delta-chk" /> Show Delta</label>
          <select id="lp-delta-gain" class="hidden" style="width:74px;font-size:11px;"><option value="4" selected>4× diff</option><option value="8">8× diff</option><option value="16">16× diff</option></select>
        </div>
      </div>
      <div id="lp-grid" class="compare-grid">
        <div id="lp-pane-orig" class="compare-pane">
          <div class="compare-label">Reference (Original)</div>
          <img id="lp-img-orig" alt="Reference frame" />
        </div>
        <div id="lp-pane-enc" class="compare-pane">
          <div class="compare-label">Test (Encoded Live)</div>
          <img id="lp-img-enc" alt="Live encoded frame" />
        </div>
        <div id="lp-pane-delta" class="compare-pane hidden">
          <div class="compare-label" id="lp-delta-lbl">Delta (Amplified 4× Diff)</div>
          <img id="lp-img-delta" alt="Delta difference frame" />
        </div>
      </div>
    `;
    openModalEl("Live Comparison Preview (Silent)", wrap, [{ label: "Hide preview", cls: "btn btn-primary", onClick: closeModal }]);

    const syncGridLayout = () => {
      const grid = $("lp-grid");
      const pOrig = $("lp-pane-orig");
      const pDelta = $("lp-pane-delta");
      if (!grid || !pOrig || !pDelta) return;
      pOrig.classList.toggle("hidden", !showSideBySide);
      pDelta.classList.toggle("hidden", !showDelta);
      grid.classList.remove("cols-1", "cols-3");
      const panesVisible = (showSideBySide ? 1 : 0) + 1 + (showDelta ? 1 : 0);
      if (panesVisible === 1) grid.classList.add("cols-1");
      else if (panesVisible === 3) grid.classList.add("cols-3");
      if ($("lp-delta-lbl")) $("lp-delta-lbl").textContent = `Delta (Amplified ${deltaGain}× Diff)`;
    };

    const sbsChk = $("lp-sbs-chk");
    const deltaChk = $("lp-delta-chk");
    const gainSel = $("lp-delta-gain");
    sbsChk?.addEventListener("change", () => { showSideBySide = sbsChk.checked; syncGridLayout(); });
    deltaChk?.addEventListener("change", () => {
      showDelta = deltaChk.checked;
      gainSel?.classList.toggle("hidden", !showDelta);
      syncGridLayout();
    });
    gainSel?.addEventListener("change", () => { deltaGain = Number(gainSel.value || 4); syncGridLayout(); });
    syncGridLayout();

    const pollOnce = async () => {
      try {
        const bundle = await invoke("get_live_comparison_bundle", {
          includeDelta: showDelta,
          deltaGain,
        });
        const li = bundle?.info;
        const ap = snapshot?.active_progress && snapshot.active_progress.job_id === jobId ? snapshot.active_progress : null;
        if ($("lp-meta")) {
          const lpParts = [];
          if (li && !li.waiting_first_frame) {
            const ts = bundle.matched_ts_secs != null ? bundle.matched_ts_secs : li.media_time_secs;
            lpParts.push(`${fmtDuration(ts)} · Frame #${li.frame_seq} · ${li.mode}`);
          } else if (li?.limitation) {
            lpParts.push(li.limitation);
          } else {
            lpParts.push("Waiting for first decodable frame…");
          }
          if (ap) {
            if (ap.bytes_written > 0) lpParts.push(`Output ${fmtBytes(ap.bytes_written)}`);
            if (ap.est_final_ratio) lpParts.push(`Est. Ratio ${ap.est_final_ratio.toFixed(2)}:1`);
            if (ap.speed) lpParts.push(`${ap.speed.toFixed(2)}x${ap.fps ? ` (${ap.fps.toFixed(1)} fps)` : ""}`);
          }
          if (showDelta && bundle?.mae_pct != null) {
            lpParts.push(`MAE ${bundle.mae_pct.toFixed(2)}% · PSNR ${bundle.psnr_db?.toFixed(1) ?? "—"} dB`);
          }
          $("lp-meta").textContent = lpParts.join(" · ");
        }
        if (bundle?.enc_data_url && $("lp-img-enc")) $("lp-img-enc").src = bundle.enc_data_url;
        if (bundle?.orig_data_url && $("lp-img-orig")) $("lp-img-orig").src = bundle.orig_data_url;
        if (showDelta && bundle?.delta_data_url && $("lp-img-delta")) $("lp-img-delta").src = bundle.delta_data_url;
      } catch (_) {}
    };

    pollOnce();
    livePreviewTimer = setInterval(pollOnce, 550);
  }

  function openTrashReviewModal(plan, onConfirm) {
    const wrap = document.createElement("div");
    const blockedNote = !plan.trash_supported && plan.trash_support_error
      ? `<div class="setup-card" style="border-color:var(--danger,#e5484d);margin-bottom:10px;"><strong style="color:var(--danger,#e5484d);">Recoverable OS Trash Unavailable</strong><div class="muted" style="font-size:12px;margin-top:4px;">${escapeHtml(plan.trash_support_error)}</div></div>`
      : "";
    const rowsHtml = (plan.items || []).map((it) => `
      <tr>
        <td><strong>${escapeHtml(it.filename)}</strong></td>
        <td class="mono muted" style="max-width:240px;word-break:break-all;">${escapeHtml(it.parent_dir)}</td>
        <td class="tabular-nums">${fmtBytes(it.size_bytes)}</td>
        <td>${it.eligible ? `<span class="badge badge-ok">Ready</span>` : `<span class="badge badge-warn">${escapeHtml(it.reason || "Blocked")}</span>`}</td>
      </tr>
    `).join("");

    wrap.innerHTML = `
      ${blockedNote}
      <p style="margin:0 0 8px;"><strong>Scope:</strong> ${escapeHtml(plan.queue_name || "Library Duplicate Selection")} · <strong>${plan.eligible_count}</strong> file${plan.eligible_count === 1 ? "" : "s"} (${fmtBytes(plan.total_bytes)}) will move to recoverable OS Trash${plan.blocked_count > 0 ? ` · <strong>${plan.blocked_count}</strong> protected/blocked` : ""}.</p>
      <div style="max-height:260px;overflow:auto;border:1px solid var(--border);border-radius:8px;">
        <table class="table-simple" style="margin:0;">
          <thead><tr><th>Filename</th><th>Folder</th><th>Size</th><th>Status</th></tr></thead>
          <tbody>${rowsHtml}</tbody>
        </table>
      </div>
    `;
    const btns = [{ label: "Cancel", cls: "btn", onClick: closeModal }];
    if (plan.eligible_count > 0 && (plan.trash_supported || plan.eligible_count > 0)) {
      btns.push({
        label: `Move ${plan.eligible_count} (${fmtBytes(plan.total_bytes)}) to OS Trash`,
        cls: "btn btn-danger",
        onClick: async () => {
          closeModal();
          await onConfirm();
        },
      });
    }
    openModalEl("Review Files Before Moving to OS Trash", wrap, btns);
  }

  async function trashOrgSelected(q) {
    const selSet = getOrgSelectedSet(q.id);
    const ids = Array.from(selSet);
    if (ids.length === 0) return;
    try {
      const reason = organizeSideTab === "duplicates" ? "queue_duplicate_group" : "queue_specific_items";
      const plan = await invoke("preview_tab_trash", {
        queueId: q.id,
        itemIds: ids,
        removalReason: reason,
      });
      openTrashReviewModal(plan, async () => {
        try {
          const res = await invoke("execute_tab_trash_plan", { planId: plan.plan_id });
          for (const o of res.outcomes || []) {
            if (o.moved) selSet.delete(o.item_id);
          }
          const firstErr = (res.outcomes || []).find((o) => !o.moved && o.error)?.error;
          if (res.moved_count > 0) {
            showToast(
              `Moved ${res.moved_count} file${res.moved_count === 1 ? "" : "s"} to recoverable OS Trash${res.failed_count > 0 ? ` (${res.failed_count} kept: ${firstErr || "blocked"})` : ""}.`,
              res.failed_count > 0 ? "warn" : "success"
            );
          } else {
            showToast(firstErr || "No files were moved to Trash.", "warn");
          }
          await refreshSnapshot();
        } catch (err) {
          showToast(String(err), "warn");
        }
      });
    } catch (err) {
      showToast(String(err), "warn");
    }
  }

  async function triggerTabWatermarkScan(q) {
    if (!q || ocrScanning) return;
    ocrScanning = true;
    renderAll();
    try {
      const count = await invoke("tab_extract_watermarks", { queueId: q.id });
      showToast(`Extracted ${count} watermark / text hit${count === 1 ? "" : "s"}.`, count > 0 ? "success" : "info");
      await loadOrganizeOverview(q.id, true);
    } catch (err) {
      showToast(String(err), "warn");
    } finally {
      ocrScanning = false;
      renderAll();
    }
  }

  async function triggerTabFaceScan(q) {
    if (!q || faceScanning) return;
    faceScanning = true;
    renderAll();
    try {
      const count = await invoke("tab_run_face_grouping", { queueId: q.id });
      showToast(`Detected ${count} face observation${count === 1 ? "" : "s"}.`, count > 0 ? "success" : "info");
      await loadOrganizeOverview(q.id, true);
    } catch (err) {
      showToast(String(err), "warn");
    } finally {
      faceScanning = false;
      renderAll();
    }
  }

  async function runMoveByTag(q, dryRun) {
    const tagName = moveSelectedTag.trim();
    const destDir = moveFolderTargetDir.trim();
    if (!tagName || !destDir) return showToast("Pick both a tag and a destination folder.", "warn");
    try {
      const planId = dryRun ? null : movePreviewResult?.plan_id || null;
      if (!dryRun && !planId) {
        return showToast("Click Preview Matching Files first to review the exact destinations.", "warn");
      }
      const res = await invoke("tab_move_by_tag", {
        queueId: q.id,
        tagName,
        destDir,
        copyOnly: moveCopyOnly,
        dryRun,
        preserveSubfolders: movePreserveSubfolders,
        collisionPolicy: moveCollisionPolicy,
        planId,
      });
      res._queueId = q.id;
      movePreviewResult = res;
      if (!dryRun) {
        const retNote = res.retained_count > 0 ? ` · ${res.retained_count} copied; original retained` : "";
        showToast(
          `${moveCopyOnly ? "Copied" : "Moved"} ${res.moved_count} file(s) tagged "${tagName}"${retNote}.`,
          res.moved_count > 0 && res.errors.length === 0 ? "success" : "warn"
        );
        await refreshSnapshot();
      } else {
        renderOrganizeView(q, activeQueueJobs());
      }
    } catch (err) {
      showToast(String(err), "warn");
    }
  }

  async function triggerTabDedupeScan(q) {
    if (!q || dedupeScanning) return;
    dedupeScanning = true;
    renderAll();
    try {
      const res = await invoke("run_tab_dedupe_scan", {
        queueId: q.id,
        alsoSha256: dedupeAlsoSha256,
        includePerceptual: dedupePerceptual,
      });
      showToast(
        `Scan finished: ${res.length} exact / similar group${res.length === 1 ? "" : "s"} found.`,
        res.length > 0 ? "success" : "info"
      );
      await loadOrganizeOverview(q.id, true);
    } catch (err) {
      showToast(String(err), "warn");
    } finally {
      dedupeScanning = false;
      await refreshSnapshot();
    }
  }

  // =========================================================================
  // 7. EVENT LISTENERS & BACKEND SUBSCRIPTIONS
  // =========================================================================
  $("open-icon-modal-btn").onclick = openIconShowcaseModal;
  $("footer-icon-btn").onclick = openIconShowcaseModal;
  $("btn-tools-status").onclick = openToolsModal;
  $("btn-new-queue").onclick = async () => {
    await invoke("create_queue_tab", { name: null, tabType: "explore" });
    if (currentPlatformView !== "transcode") {
      window.dispatchEvent(new CustomEvent("ms-request-switch-view", { detail: { view: "transcode" } }));
    }
  };

  window.addEventListener("ms-open-settings-tab", () => {
    settingsTabOpen = true;
    renderTabs();
  });
  window.addEventListener("ms-view-changed", (e) => {
    currentPlatformView = e.detail?.view || "transcode";
    if (currentPlatformView === "settings") settingsTabOpen = true;
    renderTabs();
  });
  window.addEventListener("ms-tasks-count-changed", (e) => {
    latestTasksCount = Number(e.detail?.count || 0);
    const q = activeQueue();
    if (q && activeMode(q) === "organize") renderOrganizeView(q, activeQueueJobs());
  });
  window.addEventListener("ms-faces-enabled-changed", (e) => {
    catalogFacesEnabled = Boolean(e.detail?.enabled);
    const q = activeQueue();
    if (q && activeMode(q) === "organize") renderOrganizeView(q, activeQueueJobs());
  });
  window.addEventListener("ms-open-organize-subtab", async (e) => {
    const subtab = e.detail?.subtab || "library";
    if (currentPlatformView !== "transcode") {
      window.dispatchEvent(new CustomEvent("ms-request-switch-view", { detail: { view: "transcode" } }));
    }
    const q = activeQueue();
    if (q && activeMode(q) !== "organize") {
      await invoke("set_queue_tab_type", { queueId: q.id, tabType: "organize" });
    }
    organizeSideTab = subtab;
    const aq = activeQueue();
    if (aq) {
      renderOrganizeView(aq, activeQueueJobs());
      renderActionBar();
      renderSidebar();
    }
  });
  window.addEventListener("ms-open-tab-encode", async () => {
    if (currentPlatformView !== "transcode") {
      window.dispatchEvent(new CustomEvent("ms-request-switch-view", { detail: { view: "transcode" } }));
    }
    const q = activeQueue();
    if (q) {
      setActiveEncodeSubView(q, "list");
      if (activeMode(q) !== "encode") {
        await invoke("set_queue_tab_type", { queueId: q.id, tabType: "encode" });
      }
      renderWorkspace();
      renderActionBar();
      renderSidebar();
    }
  });
  window.addEventListener("ms-open-tools-modal", openToolsModal);

  $("btn-toggle-inspector").onclick = () => {
    sidebarOpen = !sidebarOpen;
    renderSidebar();
  };
  $("sidebar-tab-setup").onclick = () => { sidebarMode = "setup"; renderSidebar(); };
  $("sidebar-tab-job").onclick = () => { sidebarMode = "job"; renderSidebar(); };

  document.querySelectorAll("#tab-mode-switcher .tab-mode-btn").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const q = activeQueue();
      if (!q) return;
      const nextMode = btn.getAttribute("data-tab-mode");
      if (activeMode(q) === nextMode) return;
      sidebarMode = nextMode === "explore" || nextMode === "repair" ? "job" : "setup";
      await invoke("set_queue_tab_type", { queueId: q.id, tabType: nextMode });
      showToast(`Switched "${q.name}" to ${(MODE_META[nextMode] || MODE_META.encode).fullLabel} mode`, "info");
    });
  });

  const pickFiles = async () => {
    const res = await dialog.open({ multiple: true });
    if (res) startImportFlow(Array.isArray(res) ? res : [res]);
  };
  const pickFolder = async () => {
    const res = await dialog.open({ directory: true, multiple: true });
    if (res) startImportFlow(Array.isArray(res) ? res : [res]);
  };
  $("btn-add-files").onclick = pickFiles;
  $("btn-empty-files").onclick = pickFiles;
  $("btn-add-folder").onclick = pickFolder;
  $("btn-empty-folder").onclick = pickFolder;

  $("btn-preview-15s").onclick = () => {
    const jobs = activeQueueJobs();
    const job = jobs.find((j) => j.id === selectedJobId) || jobs[0];
    if (job) openSamplePreviewModal(job);
  };

  $("btn-forward-tab").onclick = () => openForwardModal();

  $("btn-start-queue").onclick = async () => {
    const q = activeQueue();
    if (!q) return;
    const mode = activeMode(q);
    const jobs = activeQueueJobs();

    if (mode === "explore") {
      const comp = jobs.filter((j) => ["compress_balanced", "compress_compact"].includes(jobDecision(j)));
      return openForwardModal(comp.map((j) => j.id), "encode", "clips tagged for compression", false);
    }
    if (mode === "organize") {
      if (organizeSideTab === "watermarks") return triggerTabWatermarkScan(q);
      if (organizeSideTab === "faces") return triggerTabFaceScan(q);
      if (organizeSideTab === "folder") return runMoveByTag(q, true);
      if (getOrgSelectedSet(q.id).size > 0) return trashOrgSelected(q);
      return triggerTabDedupeScan(q);
    }
    if (mode === "repair") {
      const unrepaired = jobs.filter((j) => !["recovered", "partial_recovery"].includes(snapshot.tab_repair_items?.[j.id]?.status)).map((j) => j.id);
      if (unrepaired.length > 0) await invoke("run_tab_repair_execute", { queueId: q.id, jobIds: unrepaired });
      return;
    }

    const selectedIds = jobs.filter((j) => checkedJobIds.has(j.id) && isEncodeSelectableJob(j) && j.plan?.is_ready).map((j) => j.id);
    if (selectedIds.length === 0) {
      return showToast("Check at least one video in the list before adding to the Queue.", "warn");
    }
    try {
      const resp = await invoke("enqueue_jobs", { queueId: q.id, jobIds: selectedIds, startNow: true, consentConfirmed: null });
      if (resp && resp.requires_cleanup_consent) {
        openEnqueueConsentModal(q, resp, selectedIds);
      } else {
        selectedIds.forEach((id) => checkedJobIds.delete(id));
        showToast(`Added ${selectedIds.length} video${selectedIds.length === 1 ? "" : "s"} to the Encode Queue — processing started.`, "success");
      }
    } catch (err) {
      showToast(String(err), "warn");
    }
  };

  $("btn-view-encode-queue")?.addEventListener("click", () => {
    const q = activeQueue();
    if (!q) return;
    setActiveEncodeSubView(q, activeEncodeSubView(q) === "queue" ? "list" : "queue");
    renderWorkspace();
    renderActionBar();
  });

  $("btn-pause-after").onclick = () => invoke("pause_after_current", { queueId: activeQueue().id });
  $("btn-cancel-current").onclick = () => invoke("cancel_current_file", { jobId: null });
  $("btn-live-monitor").onclick = () => {
    const enc = activeQueueJobs().find((j) => j.status === "encoding");
    if (enc) openLivePreviewModal(enc.id);
  };
  $("btn-reset-col-sort")?.addEventListener("click", () => {
    encodeSort = { col: null, dir: "desc" };
    renderQueueList(activeQueue(), activeQueueJobs());
  });
  $("enc-search-input")?.addEventListener("input", (e) => {
    encodeFilters.search = e.target.value;
    renderQueueList(activeQueue(), activeQueueJobs());
  });
  $("enc-search-mode-seg")?.querySelectorAll("[data-search-mode]").forEach((btn) => {
    btn.addEventListener("click", () => {
      encodeFilters.searchMode = btn.getAttribute("data-search-mode");
      renderQueueList(activeQueue(), activeQueueJobs());
    });
  });
  $("btn-more-filters")?.addEventListener("click", () => {
    const dr = $("encode-more-filter-drawer");
    if (!dr) return;
    const isHidden = dr.classList.toggle("hidden");
    $("btn-more-filters").setAttribute("aria-expanded", String(!isHidden));
    syncEncodeFilterControls();
    if (!isHidden) renderFilterRulesDrawer(activeQueueJobs());
  });
  document.addEventListener("click", (e) => {
    if (activeColPopover && !activeColPopover.contains(e.target) && !e.target.closest("[data-col-flt]") && !e.target.closest("[data-edit-chip]")) {
      closeColFilterPopover();
    }
  });
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape") closeColFilterPopover();
  });
  $("btn-sel-non-modern").onclick = () => {
    const jobs = activeQueueJobs();
    checkedJobIds.clear();
    let count = 0;
    for (const j of jobs) {
      const info = jobVideoInfo(j);
      if (isEncodeSelectableJob(j) && j.probe && !isModernCodec(info.codec)) {
        checkedJobIds.add(j.id);
        count++;
      }
    }
    renderQueueList(activeQueue(), jobs);
    renderActionBar();
    renderSidebar();
    showToast(`Selected ${count} non-HEVC/AV1 file${count === 1 ? "" : "s"}.`, "info");
  };
  $("btn-desel-all").onclick = () => {
    checkedJobIds.clear();
    renderQueueList(activeQueue(), activeQueueJobs());
    renderActionBar();
    renderSidebar();
  };
  $("btn-triage-skip-hevc").onclick = async () => {
    const ids = activeQueueJobs().filter((j) => !isQueuedJob(j) && j.status === "listed" && isModernCodec(jobVideoInfo(j).codec)).map((j) => j.id);
    if (ids.length > 0) {
      ids.forEach((id) => checkedJobIds.delete(id));
      await invoke("remove_pending_jobs", { jobIds: ids });
      showToast(`Removed ${ids.length} already HEVC/AV1 file${ids.length === 1 ? "" : "s"} from tab`, "info");
    }
  };
  $("btn-retry-failed").onclick = () => {
    const ids = activeQueueJobs().filter((j) => ["failed", "canceled", "interrupted", "needs_attention"].includes(j.status)).map((j) => j.id);
    if (ids.length > 0) invoke("retry_jobs", { jobIds: ids });
  };
  $("btn-clear-completed").onclick = () => {
    const ids = activeQueueJobs().filter((j) => ["completed", "below_savings_target", "canceled"].includes(j.status)).map((j) => j.id);
    if (ids.length > 0) invoke("dequeue_jobs", { jobIds: ids });
  };

  // Segmented setup controls in sidebar
  document.querySelectorAll("[data-seg]").forEach((grp) => {
    const key = grp.getAttribute("data-seg");
    grp.querySelectorAll(".seg-btn").forEach((btn) => {
      btn.addEventListener("click", () => {
        const val = btn.getAttribute("data-val");
        patchActiveQueueSettings((p) => {
          p.recipe[key] = val;
          if (key === "family" || key === "codec" || key === "variant") applyCodecDefaults(p.recipe);
        });
      });
    });
  });

  $("sel-profile").onchange = (e) => { if (e.target.value) invoke("select_profile_for_queue", { queueId: activeQueue().id, profileId: e.target.value }); };
  $("btn-profile-menu").onclick = openProfilesModal;
  $("btn-apply-waiting").onclick = () => {
    const q = activeQueue();
    const checkedWaiting = activeQueueJobs().filter((j) => checkedJobIds.has(j.id) && ["listed", "queued", "ready", "needs_attention", "interrupted", "failed", "canceled"].includes(j.status)).map((j) => j.id);
    invoke("apply_queue_settings_to_waiting", { queueId: q.id, jobIds: checkedWaiting.length > 0 ? checkedWaiting : null });
  };

  $("rng-crf").oninput = (e) => { $("inp-crf").value = e.target.value; };
  $("rng-crf").onchange = (e) => patchActiveQueueSettings((p) => { p.recipe.crf = Number(e.target.value); });
  $("inp-crf").onchange = (e) => { $("rng-crf").value = e.target.value; patchActiveQueueSettings((p) => { p.recipe.crf = Number(e.target.value); }); };
  $("btn-reset-preset").onclick = () => patchActiveQueueSettings((p) => {
    p.recipe.tune = null;
    p.recipe.ten_bit = p.recipe.family !== "remux";
    p.recipe.aq_mode = null;
    applyCodecDefaults(p.recipe);
  });
  $("sel-tune")?.addEventListener("change", (e) => patchActiveQueueSettings((p) => {
    p.recipe.tune = e.target.value || null;
  }));
  $("sel-aq-mode")?.addEventListener("change", (e) => patchActiveQueueSettings((p) => {
    p.recipe.aq_mode = e.target.value ? Number(e.target.value) : null;
  }));
  $("chk-ten-bit")?.addEventListener("change", (e) => patchActiveQueueSettings((p) => {
    p.recipe.ten_bit = e.target.checked;
  }));
  $("btn-anime-preset-quick")?.addEventListener("click", () => {
    patchActiveQueueSettings((p) => {
      p.recipe.codec = "hevc";
      p.recipe.tune = "animation";
      p.recipe.ten_bit = true;
      p.recipe.aq_mode = 3;
      if (Number(p.recipe.crf) > 21) p.recipe.crf = 19;
    });
    showToast("Applied Anime 10-bit HEVC tuning (animation + 10-bit + AQ 3).", "success");
  });
  document.querySelectorAll("[data-help-toggle]").forEach((btn) => {
    btn.addEventListener("click", () => {
      const box = $(btn.getAttribute("data-help-toggle"));
      if (!box) return;
      const open = !box.classList.toggle("hidden");
      btn.classList.toggle("active", open);
    });
  });
  $("btn-choose-dest").onclick = async () => { const dir = await dialog.open({ directory: true }); if (dir) patchActiveQueueSettings((p) => { p.destination_root = dir; }); };
  $("btn-volume-reserve").onclick = () => {
    const vol = findQueueVolumeInfo(activeQueue());
    if (!vol) return showToast("Choose a destination folder first.", "info");
    openPromptModal("Configure Volume Reserve", `Reserve (GiB) for ${vol.mount_point}:`, (vol.reserve_bytes / (1024 ** 3)).toFixed(1), (val) => {
      invoke("set_volume_reserve", { volumeId: vol.volume_id, mountHint: vol.mount_point, reserveBytes: Math.round((Number(val) || 0) * 1024 ** 3) });
    });
  };
  $("sel-cleanup-policy").onchange = (e) => patchActiveQueueSettings((p) => { p.cleanup_policy = e.target.value; });
  $("chk-min-savings").onchange = (e) => patchActiveQueueSettings((p) => { p.min_savings_enabled = e.target.checked; });
  $("inp-min-savings-pct").onchange = (e) => patchActiveQueueSettings((p) => { p.min_savings_pct = Number(e.target.value); });
  $("sel-priority").onchange = (e) => patchActiveQueueSettings((p) => { p.priority = e.target.value; });
  $("sel-error-policy").onchange = (e) => patchActiveQueueSettings((p) => { p.error_policy = e.target.value; });
  $("chk-recursive").onchange = (e) => patchActiveQueueSettings((p) => { p.recursive_import = e.target.checked; });
  $("chk-keep-awake").onchange = (e) => patchActiveQueueSettings((p) => { p.keep_awake = e.target.checked; });
  $("chk-full-decode").onchange = (e) => patchActiveQueueSettings((p) => { p.full_decode_verify = e.target.checked; });
  $("btn-toggle-advanced").onclick = () => {
    const body = $("advanced-drawer");
    const isHidden = body.classList.toggle("hidden");
    $("btn-toggle-advanced").setAttribute("aria-expanded", String(!isHidden));
  };
  $("btn-save-named-preset").onclick = () => openPromptModal("Save Encode Preset", "Preset name:", "", (nm) => invoke("save_named_preset", { name: nm, recipe: activeQueue().recipe }));
  $("btn-delete-named-preset").onclick = () => { const id = $("sel-named-preset").value; if (id) invoke("delete_named_preset", { id }); };
  $("sel-named-preset").onchange = (e) => {
    const np = snapshot.named_presets.find((x) => x.id === e.target.value);
    $("btn-delete-named-preset").classList.toggle("hidden", !e.target.value);
    if (np) patchActiveQueueSettings((p) => { p.recipe = { ...np.recipe }; });
  };

  // Drag & Drop into active Resource Tab
  tauriWindow.getCurrentWindow().onDragDropEvent((ev) => {
    if (ev.payload.type === "over") $("empty-drop-state").classList.add("drag-over");
    else if (ev.payload.type === "leave") $("empty-drop-state").classList.remove("drag-over");
    else if (ev.payload.type === "drop") {
      $("empty-drop-state").classList.remove("drag-over");
      startImportFlow(ev.payload.paths);
    }
  });

  // Safe Close Guard
  let forceAllowClose = false;
  tauriWindow.getCurrentWindow().onCloseRequested(async (ev) => {
    if (forceAllowClose) return;
    ev.preventDefault();
    try {
      const busy = await invoke("check_close_status");
      if (!busy) { forceAllowClose = true; return tauriWindow.getCurrentWindow().close(); }
      openModal("Encoding Still in Progress", `<p><strong>Media Shack</strong> is currently processing jobs.</p>`, [
        { label: "Keep Running (Minimize)", cls: "btn btn-primary", onClick: async () => { closeModal(); await tauriWindow.getCurrentWindow().minimize(); } },
        { label: "Stop and Quit", cls: "btn btn-danger", onClick: () => invoke("stop_and_quit") },
        { label: "Stay Here", cls: "btn", onClick: closeModal },
      ]);
    } catch (_) {
      forceAllowClose = true;
      tauriWindow.getCurrentWindow().close();
    }
  });

  // Cross-platform Chrome-style overlay titlebar (tauri-plugin-decorum)
  function initTitlebarChrome() {
    const ua = navigator.userAgent || "";
    const isWindows = ua.includes("Windows");
    const isMac = !isWindows && (ua.includes("Mac") || ua.includes("Macintosh"));
    document.body.classList.toggle("os-windows", isWindows);
    document.body.classList.toggle("os-macos", isMac);
    document.body.classList.toggle("os-other", !isWindows && !isMac);

    const header = $("chrome-header");
    const curWin = tauriWindow?.getCurrentWindow?.();
    if (!curWin) return;

    if (isMac) {
      const setFs = (fs) => document.body.classList.toggle("is-fullscreen", Boolean(fs));
      event.listen("will-enter-fullscreen", () => setFs(true));
      event.listen("did-enter-fullscreen", () => setFs(true));
      event.listen("will-exit-fullscreen", () => setFs(false));
      event.listen("did-exit-fullscreen", () => setFs(false));
      if (typeof curWin.isFullscreen === "function") {
        curWin.isFullscreen().then(setFs).catch(() => {});
      }
      if (typeof curWin.onResized === "function") {
        curWin.onResized(() => {
          if (typeof curWin.isFullscreen === "function") {
            curWin.isFullscreen().then(setFs).catch(() => {});
          }
        });
      }
    }

    if (header) {
      const interactiveSel = "button, a, input, select, textarea, .tab-item, .chrome-tab, .utility-menu, .window-controls-slot";
      header.addEventListener("mousedown", (e) => {
        if (e.button !== 0 || e.detail > 1) return;
        if (e.target.hasAttribute?.("data-tauri-drag-region")) return;
        if (e.target.closest?.(interactiveSel)) return;
        curWin.startDragging?.().catch(() => {});
      });
      header.addEventListener("dblclick", (e) => {
        if (e.target.closest?.(interactiveSel)) return;
        curWin.toggleMaximize?.().catch(() => {});
      });
    }
  }
  initTitlebarChrome();

  event.listen("import_scan_progress", (e) => {
    latestImportProgress = e.payload;
    updateScanProgressDom();
  });
  event.listen("probe_stats_progress", (e) => {
    const p = e.payload;
    if (!p || !p.queue_id) return;
    if (p.done >= p.total) statsProgressByQueue.delete(p.queue_id);
    else statsProgressByQueue.set(p.queue_id, p);
    renderLineageBar();
  });
  event.listen("snapshot_changed", refreshSnapshot);
  event.listen("job_progress", (e) => {
    if (!snapshot) return;
    snapshot.active_progress = e.payload;
    recordLiveSpeedPoint(e.payload);
    const aq = activeQueue();
    if (aq && activeMode(aq) === "encode") {
      if (activeEncodeSubView(aq) === "queue" && e.payload?.queue_id === aq.id) {
        updateQueuePageProgressDom(e.payload);
      } else if (activeEncodeSubView(aq) === "list") {
        updateEncodeProgressDom(e.payload);
      }
    }
  });
  event.listen("sample_preview_progress", async () => {
    snapshot = await invoke("get_snapshot");
    if (openSampleRefresh) openSampleRefresh();
  });
  event.listen("repair_progress", (e) => {
    if (e.payload?.job_id && e.payload?.progress) {
      repairLiveProgress.set(e.payload.job_id, e.payload.progress);
      if (activeMode(activeQueue()) === "repair") renderRepairView(activeQueue(), activeQueueJobs());
    }
  });

  // Custom themed dropdowns replacing macOS WKWebView native NSMenu popups
  function initCustomSelects() {
    let openMenu = null;
    let openTriggerBtn = null;

    const closeOpenMenu = () => {
      if (openMenu) {
        openMenu.remove();
        openMenu = null;
      }
      if (openTriggerBtn) {
        openTriggerBtn.classList.remove("open");
        openTriggerBtn.setAttribute("aria-expanded", "false");
        openTriggerBtn = null;
      }
    };

    const nativeValDesc = Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value");
    if (nativeValDesc && !HTMLSelectElement.prototype._msPatched) {
      HTMLSelectElement.prototype._msPatched = true;
      Object.defineProperty(HTMLSelectElement.prototype, "value", {
        configurable: true,
        get() { return nativeValDesc.get.call(this); },
        set(v) {
          nativeValDesc.set.call(this, v);
          if (typeof this._msSync === "function") this._msSync();
        },
      });
      const nativeIdxDesc = Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "selectedIndex");
      if (nativeIdxDesc) {
        Object.defineProperty(HTMLSelectElement.prototype, "selectedIndex", {
          configurable: true,
          get() { return nativeIdxDesc.get.call(this); },
          set(v) {
            nativeIdxDesc.set.call(this, v);
            if (typeof this._msSync === "function") this._msSync();
          },
        });
      }
    }

    const enhance = (sel) => {
      if (!sel || sel.dataset.msEnhanced === "1" || sel.multiple || (sel.size && sel.size > 1)) return;
      if (sel.classList.contains("hidden")) return;
      sel.dataset.msEnhanced = "1";

      const isFull = sel.classList.contains("full-input") || sel.style.width === "100%";
      const wrap = document.createElement("div");
      wrap.className = "ms-select-wrap" + (isFull ? " ms-full" : "");
      if (sel.style.width && !isFull) wrap.style.width = sel.style.width;
      if (sel.style.minWidth) wrap.style.minWidth = sel.style.minWidth;

      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = "ms-select-btn";
      btn.setAttribute("aria-haspopup", "listbox");
      btn.setAttribute("aria-expanded", "false");
      if (sel.title) btn.title = sel.title;

      const lbl = document.createElement("span");
      lbl.className = "ms-select-label";
      btn.appendChild(lbl);
      btn.insertAdjacentHTML("beforeend", `<svg class="ms-select-chev" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><path d="m6 9 6 6 6-6"/></svg>`);

      const sync = () => {
        const chosen = sel.options[sel.selectedIndex] || sel.options[0];
        lbl.textContent = chosen ? chosen.textContent : "—";
        btn.disabled = Boolean(sel.disabled);
        wrap.classList.toggle("hidden", sel.classList.contains("hidden"));
      };
      sel._msSync = sync;

      sel.parentNode.insertBefore(wrap, sel);
      wrap.appendChild(btn);
      wrap.appendChild(sel);
      sel.classList.add("ms-select-native-hidden");
      sync();

      sel.addEventListener("change", sync);
      const optObs = new MutationObserver(sync);
      optObs.observe(sel, { childList: true, subtree: true, characterData: true, attributes: true, attributeFilter: ["disabled", "class"] });

      btn.addEventListener("click", (ev) => {
        ev.stopPropagation();
        if (btn.disabled) return;
        const wasSame = openTriggerBtn === btn;
        closeOpenMenu();
        if (wasSame) return;

        const rect = btn.getBoundingClientRect();
        const menu = document.createElement("div");
        menu.className = "ms-select-menu";
        menu.setAttribute("role", "listbox");

        Array.from(sel.options).forEach((opt) => {
          const item = document.createElement("button");
          item.type = "button";
          item.className = "ms-select-opt" + (opt.value === sel.value ? " active" : "");
          item.disabled = Boolean(opt.disabled);
          item.setAttribute("role", "option");
          item.setAttribute("aria-selected", opt.value === sel.value ? "true" : "false");
          item.innerHTML = `<span class="truncate">${escapeHtml(opt.textContent)}</span>${opt.value === sel.value ? `<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><polyline points="20 6 9 17 4 12"/></svg>` : ""}`;
          item.addEventListener("click", (e) => {
            e.stopPropagation();
            if (opt.disabled) return;
            const changed = sel.value !== opt.value;
            sel.value = opt.value;
            closeOpenMenu();
            if (changed) sel.dispatchEvent(new Event("change", { bubbles: true }));
          });
          menu.appendChild(item);
        });

        document.body.appendChild(menu);
        menu.style.minWidth = `${Math.max(rect.width, 150)}px`;
        const mRect = menu.getBoundingClientRect();
        const spaceBelow = window.innerHeight - rect.bottom - 8;
        const top = (spaceBelow >= mRect.height || spaceBelow >= rect.top)
          ? Math.min(window.innerHeight - mRect.height - 8, rect.bottom + 4)
          : Math.max(8, rect.top - mRect.height - 4);
        const left = Math.min(Math.max(8, rect.left), window.innerWidth - mRect.width - 8);
        menu.style.top = `${top}px`;
        menu.style.left = `${left}px`;

        btn.classList.add("open");
        btn.setAttribute("aria-expanded", "true");
        openMenu = menu;
        openTriggerBtn = btn;
      });
    };

    document.addEventListener("click", closeOpenMenu);
    window.addEventListener("resize", closeOpenMenu);
    document.addEventListener("keydown", (e) => {
      if (e.key === "Escape") closeOpenMenu();
    });

    document.querySelectorAll("select").forEach(enhance);
    const rootObs = new MutationObserver((muts) => {
      for (const m of muts) {
        for (const node of m.addedNodes) {
          if (node.nodeType !== 1) continue;
          if (node.tagName === "SELECT") enhance(node);
          else if (node.querySelectorAll) node.querySelectorAll("select").forEach(enhance);
        }
      }
    });
    rootObs.observe(document.body, { childList: true, subtree: true });
  }

  initCustomSelects();
  refreshSnapshot();
})();
