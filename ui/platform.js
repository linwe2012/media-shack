(() => {
  const { core, event, dialog } = window.__TAURI__;
  const invoke = core.invoke;
  const $ = (id) => document.getElementById(id);

  // Shared platform state & selection across Resource Tabs, Library, Organize, Duplicates, Repair
  const state = {
    currentView: "transcode",
    theme: localStorage.getItem("ms_theme") || "auto",
    viewMode: "table", // "table" | "grid"
    roots: [],
    volumes: [],
    collections: [],
    tags: [],
    catalogPage: { items: [], total_count: 0, total_bytes: 0, online_count: 0, offline_count: 0 },
    selectedFileKeys: new Set(),
    activeFileKey: null,
    lastClickedIndex: -1,
    activeCollectionId: null,
    filter: Object.assign({
      search_text: "",
      root_id: null,
      directory_prefix: null,
      include_tags: [],
      exclude_tags: [],
      tag_match_mode: "and",
      media_type: "all",
      codec: null,
      min_width: null,
      min_size_bytes: null,
      min_duration: null,
      max_duration: null,
      creator: null,
      ocr_query: null,
      integrity_status: null,
      vr_only: false,
      only_duplicates: false,
      only_untagged: false,
      only_watermarked: false,
      online_only: null,
      sort_by: "filename",
      sort_dir: "asc",
      limit: 200,
      offset: 0,
    }, JSON.parse(localStorage.getItem("ms_lib_filter") || "{}")),
    columns: JSON.parse(localStorage.getItem("ms_lib_cols") || '["filename","media_type","resolution","codec","duration","size","integrity","tags"]'),
    semanticResults: null,
    dupGroups: [],
    selectedDupGroupId: null,
    orgMode: "rename", // rename | move | llm | history
    orgPreview: null,
    faceGroups: [],
    selectedFaceGroupId: null,
    tasks: [],
    budgets: null,
    repairDiagnosis: null,
    updateCheck: null,
    updateChecking: false,
    updateError: null,
    updateStatusMessage: null,
    updateDownloading: false,
    updateProgress: null,
    updateDownloadedPath: null,
    autoUpdateCheck: localStorage.getItem("ms_auto_update_check") !== "false",
  };

  function esc(s) {
    if (s === null || s === undefined) return "";
    return String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
  }

  function fmtBytes(bytes) {
    if (bytes === null || bytes === undefined) return "—";
    if (bytes === 0) return "0 B";
    const u = ["B", "KiB", "MiB", "GiB", "TiB"];
    let v = bytes;
    let i = 0;
    while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
    return `${v.toFixed(i === 0 ? 0 : v >= 100 ? 1 : 2)} ${u[i]}`;
  }

  function fmtDur(secs) {
    if (!secs || !isFinite(secs)) return "—";
    const s = Math.floor(secs);
    const h = Math.floor(s / 3600);
    const m = Math.floor((s % 3600) / 60);
    const r = s % 60;
    return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${String(r).padStart(2, "0")}` : `${m}:${String(r).padStart(2, "0")}`;
  }

  function thumbUrlFromAbs(absPath) {
    if (!absPath) return null;
    const norm = absPath.replace(/\\/g, "/");
    const marker = "/analysis_cache/";
    const idx = norm.indexOf(marker);
    if (idx === -1) return null;
    const rel = norm.slice(idx + marker.length);
    return `thumb://localhost/${rel.split("/").map(encodeURIComponent).join("/")}`;
  }

  // Accent tone management (neutral / amber / sage / custom hex)
  const PRESET_ACCENT_HEX = {
    neutral: "#71717A",
    amber: "#F59E0B",
    sage: "#5FA78E",
  };
  let currentAccent = localStorage.getItem("ms_accent") || "neutral";
  let currentCustomHex = localStorage.getItem("ms_custom_accent") || "#3B82F6";
  if (!["neutral", "amber", "sage", "custom"].includes(currentAccent)) currentAccent = "neutral";

  function normalizeHex(hex) {
    if (!hex) return "#3B82F6";
    const m = String(hex).trim().match(/^#?([0-9a-fA-F]{6})$/);
    return m ? `#${m[1].toUpperCase()}` : "#3B82F6";
  }

  function hexToRgb(hex) {
    const clean = normalizeHex(hex).slice(1);
    return {
      r: parseInt(clean.slice(0, 2), 16),
      g: parseInt(clean.slice(2, 4), 16),
      b: parseInt(clean.slice(4, 6), 16),
    };
  }

  function rgbToHex(r, g, b) {
    const clamp = (n) => Math.max(0, Math.min(255, Math.round(n)));
    return `#${[clamp(r), clamp(g), clamp(b)].map((v) => v.toString(16).padStart(2, "0")).join("").toUpperCase()}`;
  }

  function mixRgb(a, b, t) {
    return {
      r: a.r + (b.r - a.r) * t,
      g: a.g + (b.g - a.g) * t,
      b: a.b + (b.b - a.b) * t,
    };
  }

  function isEffectiveDark() {
    if (state.theme === "dark") return true;
    if (state.theme === "light") return false;
    return window.matchMedia && window.matchMedia("(prefers-color-scheme: dark)").matches;
  }

  function clearCustomAccentVars() {
    const st = document.documentElement.style;
    ["--accent", "--accent-hover", "--accent-solid", "--accent-solid-text", "--accent-soft", "--accent-border", "--accent-text", "--accent-progress"].forEach((k) => st.removeProperty(k));
  }

  function applyCustomAccentVars(hex) {
    const norm = normalizeHex(hex);
    const rgb = hexToRgb(norm);
    const lum = (0.299 * rgb.r + 0.587 * rgb.g + 0.114 * rgb.b) / 255;
    const dark = isEffectiveDark();
    const st = document.documentElement.style;
    const white = { r: 255, g: 255, b: 255 };
    const black = { r: 15, g: 15, b: 18 };
    const hoverRgb = dark ? mixRgb(rgb, white, 0.2) : mixRgb(rgb, black, 0.16);
    const textRgb = dark ? (lum < 0.45 ? mixRgb(rgb, white, 0.42) : rgb) : (lum > 0.62 ? mixRgb(rgb, black, 0.45) : rgb);
    const hoverHex = rgbToHex(hoverRgb.r, hoverRgb.g, hoverRgb.b);
    const textHex = rgbToHex(textRgb.r, textRgb.g, textRgb.b);

    st.setProperty("--accent", norm);
    st.setProperty("--accent-hover", hoverHex);
    st.setProperty("--accent-solid", norm);
    st.setProperty("--accent-solid-text", lum > 0.60 ? "#141414" : "#FFFFFF");
    document.documentElement.setAttribute("data-accent-bright", lum > 0.60 ? "true" : "false");
    st.setProperty("--accent-soft", `rgba(${rgb.r}, ${rgb.g}, ${rgb.b}, ${dark ? 0.16 : 0.12})`);
    st.setProperty("--accent-border", `rgba(${rgb.r}, ${rgb.g}, ${rgb.b}, ${dark ? 0.42 : 0.36})`);
    st.setProperty("--accent-text", textHex);
    st.setProperty("--accent-progress", `linear-gradient(90deg, ${norm} 0%, ${hoverHex} 100%)`);
  }

  function applyAccent(accent, customHex = null) {
    if (customHex) currentCustomHex = normalizeHex(customHex);
    currentAccent = ["neutral", "amber", "sage", "custom"].includes(accent) ? accent : "neutral";
    localStorage.setItem("ms_accent", currentAccent);
    localStorage.setItem("ms_custom_accent", currentCustomHex);
    document.documentElement.setAttribute("data-accent", currentAccent);
    if (currentAccent === "custom") {
      applyCustomAccentVars(currentCustomHex);
    } else {
      clearCustomAccentVars();
      document.documentElement.removeAttribute("data-accent-bright");
    }
    document.querySelectorAll("[data-accent-pick]").forEach((btn) => {
      const val = btn.getAttribute("data-accent-pick");
      const isSelected = val === currentAccent;
      btn.classList.toggle("active", isSelected);
      btn.setAttribute("aria-pressed", isSelected ? "true" : "false");
    });
    const customHexLbl = $("lbl-custom-accent-hex");
    if (customHexLbl) {
      const displayHex = currentAccent === "custom" ? currentCustomHex : (PRESET_ACCENT_HEX[currentAccent] || currentCustomHex);
      customHexLbl.textContent = displayHex;
    }
    const pickerInp = $("inp-custom-accent");
    if (pickerInp && pickerInp.value.toUpperCase() !== currentCustomHex) {
      pickerInp.value = currentCustomHex;
    }
  }
  applyAccent(currentAccent);

  // Theme management (auto / dark / light)
  function applyTheme() {
    if (state.theme === "dark" || state.theme === "light") {
      document.documentElement.setAttribute("data-theme", state.theme);
    } else {
      document.documentElement.removeAttribute("data-theme");
    }
    document.querySelectorAll("[data-theme-pick]").forEach((btn) => {
      const isSelected = btn.getAttribute("data-theme-pick") === state.theme;
      btn.classList.toggle("active", isSelected);
      btn.setAttribute("aria-pressed", isSelected ? "true" : "false");
    });
    if (currentAccent === "custom") applyCustomAccentVars(currentCustomHex);
  }
  applyTheme();
  if (window.matchMedia) {
    window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", () => {
      if (currentAccent === "custom") applyCustomAccentVars(currentCustomHex);
    });
  }

  // Primary navigation (Resource Tabs + Settings)
  const openSettingsTabBtn = $("btn-open-settings-tab");
  const updatePillBtn = $("btn-update-pill");
  if (updatePillBtn) {
    updatePillBtn.addEventListener("click", () => {
      window.dispatchEvent(new CustomEvent("ms-open-settings-tab"));
      switchView("settings");
    });
  }
  if (openSettingsTabBtn) {
    openSettingsTabBtn.addEventListener("click", () => {
      window.dispatchEvent(new CustomEvent("ms-open-settings-tab"));
      switchView("settings");
    });
  }

  function switchView(view) {
    const next = view === "settings" ? "settings" : "transcode";
    state.currentView = next;
    if (openSettingsTabBtn) openSettingsTabBtn.classList.toggle("active", next === "settings");
    document.querySelectorAll(".nav-view").forEach((el) => {
      el.classList.toggle("hidden", el.id !== `view-${next}`);
    });
    window.dispatchEvent(new CustomEvent("ms-view-changed", { detail: { view: next } }));
    if (next === "settings") refreshSettingsView();
  }

  window.addEventListener("ms-request-switch-view", (e) => {
    if (e.detail?.view) switchView(e.detail.view);
  });

  window.addEventListener("ms-org-tool-activated", (e) => {
    const tool = e.detail?.tool;
    if (tool === "library") refreshLibrary();
    if (tool === "catalog_dups") refreshDuplicates();
    if (tool === "batch") renderOrganizeView();
    if (tool === "catalog_people") refreshPeopleView();
    if (tool === "integrity") refreshRepairView();
    if (tool === "tasks") refreshTasksView();
  });

  function openOrganizeSubtab(subtab) {
    window.dispatchEvent(new CustomEvent("ms-open-organize-subtab", { detail: { subtab } }));
  }

  function updateSelectionPill() {
    const cnt = state.selectedFileKeys.size;
    const libPill = $("lbl-lib-selected");
    if (libPill) {
      libPill.textContent = `${cnt} selected`;
      libPill.classList.toggle("hidden", cnt === 0);
    }
    if ($("lbl-org-selected")) {
      $("lbl-org-selected").textContent = `${cnt} files selected from Library`;
    }
  }

  // Resizable panels
  document.querySelectorAll(".resizer-v").forEach((handle) => {
    handle.addEventListener("mousedown", (ev) => {
      ev.preventDefault();
      const target = $(handle.dataset.target);
      const side = handle.dataset.side;
      const startX = ev.clientX;
      const startW = target.getBoundingClientRect().width;
      const onMove = (e) => {
        const dx = e.clientX - startX;
        const nextW = side === "left" ? startW + dx : startW - dx;
        target.style.width = `${Math.max(160, Math.min(540, nextW))}px`;
      };
      const onUp = () => {
        window.removeEventListener("mousemove", onMove);
        window.removeEventListener("mouseup", onUp);
      };
      window.addEventListener("mousemove", onMove);
      window.addEventListener("mouseup", onUp);
    });
  });

  // =========================================================================
  // LIBRARY VIEW
  // =========================================================================

  async function refreshLibrary() {
    try {
      const [roots, vols, cols, tags, stats, faceEnabled] = await Promise.all([
        invoke("catalog_list_roots"),
        invoke("catalog_refresh_volumes").catch(() => []),
        invoke("collections_list"),
        invoke("tags_list"),
        invoke("catalog_get_stats"),
        invoke("faces_get_enabled").catch(() => false),
      ]);
      state.roots = roots;
      state.volumes = vols;
      state.collections = cols;
      state.tags = tags;
      window.dispatchEvent(new CustomEvent("ms-faces-enabled-changed", { detail: { enabled: Boolean(faceEnabled) } }));

      $("lbl-catalog-footer").textContent =
        `Catalog: ${stats.total_files} files (${stats.online_files} online, ${stats.offline_files} offline) · ${fmtBytes(stats.total_bytes)} · ${stats.total_tags} tags`;

      if ($("chk-lib-semantic").checked && state.filter.search_text.trim()) {
        state.semanticResults = await invoke("semantic_search", {
          query: state.filter.search_text.trim(),
          filter: state.filter,
          limit: 100,
        });
      } else {
        state.semanticResults = null;
        state.catalogPage = await invoke("catalog_query", { filter: state.filter });
      }
      renderLibrarySidebar();
      renderLibraryItems();
      updateSelectionPill();
    } catch (err) {
      console.error("refreshLibrary error:", err);
    }
  }

  function renderLibrarySidebar() {
    const volList = $("lib-volumes-list");
    volList.innerHTML = "";
    for (const v of state.volumes) {
      const row = document.createElement("div");
      row.className = "sb-item";
      row.innerHTML = `<span><span class="dot ${v.is_online ? "ok" : "err"}" style="display:inline-block;margin-right:5px"></span>${esc(v.mount_point)}</span><span class="muted">${v.is_online ? fmtBytes(v.free_bytes) : "Offline"}</span>`;
      volList.appendChild(row);
    }

    const rootList = $("lib-roots-list");
    rootList.innerHTML = "";
    const allBtn = document.createElement("button");
    allBtn.type = "button";
    allBtn.className = "sb-item" + (!state.filter.root_id && !state.activeCollectionId ? " active" : "");
    allBtn.textContent = "All Indexed Media";
    allBtn.onclick = () => {
      state.filter.root_id = null;
      state.activeCollectionId = null;
      state.filter.offset = 0;
      refreshLibrary();
    };
    rootList.appendChild(allBtn);

    for (const r of state.roots) {
      const wrap = document.createElement("div");
      wrap.className = "sb-item" + (state.filter.root_id === r.id ? " active" : "");
      const lbl = document.createElement("span");
      lbl.textContent = r.label || r.path;
      lbl.title = r.path;
      lbl.style.flex = "1";
      lbl.style.overflow = "hidden";
      lbl.style.textOverflow = "ellipsis";
      lbl.onclick = () => {
        state.filter.root_id = r.id;
        state.activeCollectionId = null;
        state.filter.offset = 0;
        refreshLibrary();
      };
      const scanBtn = document.createElement("button");
      scanBtn.type = "button";
      scanBtn.className = "btn btn-ghost btn-sm";
      scanBtn.textContent = "↻";
      scanBtn.title = "Rescan root";
      scanBtn.onclick = async (e) => {
        e.stopPropagation();
        await invoke("catalog_scan_root", { rootId: r.id, targetLevel: 1 });
        refreshTasksView();
      };
      wrap.appendChild(lbl);
      wrap.appendChild(scanBtn);
      rootList.appendChild(wrap);
    }

    const colList = $("lib-collections-list");
    colList.innerHTML = "";
    for (const c of state.collections) {
      const row = document.createElement("div");
      row.className = "sb-item" + (state.activeCollectionId === c.id ? " active" : "");
      const lbl = document.createElement("span");
      lbl.textContent = c.name;
      lbl.style.flex = "1";
      lbl.onclick = () => {
        state.activeCollectionId = c.id;
        state.filter = { ...state.filter, ...c.filter, offset: 0 };
        localStorage.setItem("ms_lib_filter", JSON.stringify(state.filter));
        refreshLibrary();
      };
      row.appendChild(lbl);
      if (!c.is_builtin) {
        const del = document.createElement("button");
        del.type = "button";
        del.className = "btn btn-ghost btn-sm";
        del.textContent = "×";
        del.title = "Delete collection";
        del.onclick = async (ev) => {
          ev.stopPropagation();
          await invoke("collections_delete", { id: c.id });
          if (state.activeCollectionId === c.id) state.activeCollectionId = null;
          refreshLibrary();
        };
        row.appendChild(del);
      }
      colList.appendChild(row);
    }

    const tagList = $("lib-tags-list");
    tagList.innerHTML = "";
    for (const t of state.tags.slice(0, 40)) {
      const active = state.filter.include_tags.includes(t.name);
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = "sb-item" + (active ? " active" : "");
      btn.innerHTML = `<span>${esc(t.name)}</span><span class="muted">${t.usage_count}</span>`;
      btn.onclick = () => {
        if (active) {
          state.filter.include_tags = state.filter.include_tags.filter((x) => x !== t.name);
        } else {
          state.filter.include_tags = [...state.filter.include_tags, t.name];
        }
        state.filter.offset = 0;
        refreshLibrary();
      };
      tagList.appendChild(btn);
    }
  }

  function renderLibraryItems() {
    const items = state.semanticResults
      ? state.semanticResults.map((h) => ({
          file_key: h.file_key,
          asset_id: h.asset_id,
          filename: h.filename,
          path: h.path,
          media_type: "video",
          width: null,
          height: null,
          video_codec: null,
          duration_secs: h.best_timestamp_secs,
          size_bytes: 0,
          integrity_status: `score ${(h.score * 100).toFixed(0)}% @ ${fmtDur(h.best_timestamp_secs)}`,
          tags: [],
          thumbnail_path: h.thumbnail_path,
          is_online: true,
        }))
      : state.catalogPage.items;

    $("lib-table-wrap").classList.toggle("hidden", state.viewMode !== "table");
    $("lib-grid-wrap").classList.toggle("hidden", state.viewMode !== "grid");

    if (state.viewMode === "table") {
      const colLabels = {
        filename: "Filename",
        media_type: "Type",
        resolution: "Resolution",
        codec: "Codec",
        duration: "Duration",
        size: "Size",
        bitrate: "Bitrate",
        integrity: "Integrity",
        tags: "Tags",
        path: "Path",
      };
      const thead = $("lib-thead");
      thead.innerHTML = `<tr><th style="width:26px"><input type="checkbox" id="chk-lib-head-all" /></th>${state.columns
        .map((c) => `<th data-sort="${c}">${esc(colLabels[c] || c)}</th>`)
        .join("")}</tr>`;

      thead.querySelectorAll("th[data-sort]").forEach((th) => {
        th.onclick = () => {
          const s = th.dataset.sort;
          const mapSort = { size: "size", duration: "duration", resolution: "resolution", codec: "codec", bitrate: "bitrate", filename: "filename" };
          if (!mapSort[s]) return;
          if (state.filter.sort_by === mapSort[s]) {
            state.filter.sort_dir = state.filter.sort_dir === "asc" ? "desc" : "asc";
          } else {
            state.filter.sort_by = mapSort[s];
            state.filter.sort_dir = "asc";
          }
          refreshLibrary();
        };
      });
      $("chk-lib-head-all").onchange = (e) => {
        if (e.target.checked) items.forEach((it) => state.selectedFileKeys.add(it.file_key));
        else items.forEach((it) => state.selectedFileKeys.delete(it.file_key));
        renderLibraryItems();
        updateSelectionPill();
      };

      const tbody = $("lib-tbody");
      tbody.innerHTML = "";
      items.forEach((it, idx) => {
        const tr = document.createElement("tr");
        const isSel = state.selectedFileKeys.has(it.file_key);
        tr.className = (isSel ? "selected " : "") + (!it.is_online ? "offline-row" : "");
        const resStr = it.width && it.height ? `${it.width}×${it.height}` : "—";
        const tagsStr = (it.tags || []).map((t) => t.name).join(", ");
        const cells = {
          filename: `<td title="${esc(it.path)}">${esc(it.filename)}${!it.is_online ? ' <span class="badge badge-warn">Offline</span>' : ""}</td>`,
          media_type: `<td>${esc(it.media_type)}</td>`,
          resolution: `<td>${esc(resStr)}</td>`,
          codec: `<td>${esc(it.video_codec || "—")}</td>`,
          duration: `<td>${state.semanticResults ? esc(it.integrity_status) : fmtDur(it.duration_secs)}</td>`,
          size: `<td>${it.size_bytes ? fmtBytes(it.size_bytes) : "—"}</td>`,
          bitrate: `<td>${it.bitrate ? `${Math.round(it.bitrate / 1000)} kbps` : "—"}</td>`,
          integrity: `<td>${esc(it.integrity_status)}</td>`,
          tags: `<td title="${esc(tagsStr)}">${esc(tagsStr || "—")}</td>`,
          path: `<td class="mono" title="${esc(it.path)}">${esc(it.path)}</td>`,
        };
        tr.innerHTML = `<td><input type="checkbox" ${isSel ? "checked" : ""} /></td>${state.columns.map((c) => cells[c] || "<td>—</td>").join("")}`;
        tr.onclick = (e) => handleRowSelect(it, idx, items, e);
        tr.oncontextmenu = (e) => {
          e.preventDefault();
          if (!state.selectedFileKeys.has(it.file_key)) {
            state.selectedFileKeys.clear();
            state.selectedFileKeys.add(it.file_key);
            state.activeFileKey = it.file_key;
            renderLibraryItems();
            updateSelectionPill();
          }
          openLibraryContextMenu(e.clientX, e.clientY, it);
        };
        tbody.appendChild(tr);
      });
    } else {
      const grid = $("lib-grid-wrap");
      grid.innerHTML = "";
      items.forEach((it, idx) => {
        const card = document.createElement("div");
        const isSel = state.selectedFileKeys.has(it.file_key);
        card.className = "thumb-card" + (isSel ? " selected" : "");
        const turl = thumbUrlFromAbs(it.thumbnail_path);
        const mediaHtml = turl
          ? `<img src="${turl}" alt="${esc(it.filename)}" loading="lazy" />`
          : `<div class="thumb-placeholder">${esc(it.video_codec || it.media_type)}</div>`;
        card.innerHTML = `${mediaHtml}<div class="thumb-meta"><div class="thumb-title" title="${esc(it.filename)}">${esc(it.filename)}</div><div class="muted">${it.width ? `${it.width}×${it.height}` : ""} · ${fmtDur(it.duration_secs)}</div></div>`;
        card.onclick = (e) => handleRowSelect(it, idx, items, e);
        card.oncontextmenu = (e) => {
          e.preventDefault();
          openLibraryContextMenu(e.clientX, e.clientY, it);
        };
        grid.appendChild(card);
      });
    }

    if (state.semanticResults) {
      $("lib-page-info").textContent = `${items.length} semantic visual matches`;
    } else {
      const p = state.catalogPage;
      $("lib-page-info").textContent = `${p.items.length} of ${p.total_count} items · ${fmtBytes(p.total_bytes)}`;
    }
  }

  function handleRowSelect(item, idx, items, ev) {
    if (ev.shiftKey && state.lastClickedIndex >= 0) {
      const lo = Math.min(state.lastClickedIndex, idx);
      const hi = Math.max(state.lastClickedIndex, idx);
      for (let i = lo; i <= hi; i++) state.selectedFileKeys.add(items[i].file_key);
    } else if (ev.ctrlKey || ev.metaKey || ev.target.type === "checkbox") {
      if (state.selectedFileKeys.has(item.file_key)) state.selectedFileKeys.delete(item.file_key);
      else state.selectedFileKeys.add(item.file_key);
    } else {
      state.selectedFileKeys.clear();
      state.selectedFileKeys.add(item.file_key);
    }
    state.lastClickedIndex = idx;
    state.activeFileKey = item.file_key;
    updateSelectionPill();
    renderLibraryItems();
    loadInspector(item.file_key);
  }

  async function loadInspector(fileKey) {
    const box = $("lib-inspector-content");
    try {
      const detail = await invoke("catalog_get_asset_detail", { fileKey });
      const e = detail.entry;
      const turl = thumbUrlFromAbs(e.thumbnail_path);
      const tagChips = (e.tags || [])
        .map((t) => `<span class="tag-chip ${t.source === "ai_suggested" ? "ai" : ""}">${esc(t.name)}<button data-rm-tag="${esc(t.id)}" title="Remove tag">×</button></span>`)
        .join("");
      const relHtml = detail.relationships.length
        ? detail.relationships.map((r) => `<div class="muted">${esc(r.relationship_type)}</div>`).join("")
        : `<span class="muted">None</span>`;
      const ocrHtml = detail.ocr_snippets.length
        ? detail.ocr_snippets.map((o) => `<span class="tag-chip">${esc(o)}</span>`).join("")
        : `<span class="muted">No OCR text extracted</span>`;

      box.innerHTML = `
        <div class="side-section">
          ${turl ? `<img src="${turl}" style="width:100%;max-height:160px;object-fit:contain;border-radius:6px;background:#000;margin-bottom:8px" />` : ""}
          <h3 style="word-break:break-all">${esc(e.filename)}</h3>
          <div class="mono muted" style="word-break:break-all;margin-bottom:6px">${esc(e.path)}</div>
          <div class="inline-row">
            <button id="insp-reveal" class="btn btn-ghost btn-sm" type="button">Reveal</button>
            <button id="insp-open" class="btn btn-ghost btn-sm" type="button">Play</button>
            <button id="insp-verify" class="btn btn-ghost btn-sm" type="button">Verify</button>
            <button id="insp-repair" class="btn btn-ghost btn-sm" type="button">Repair</button>
          </div>
        </div>
        <div class="side-section">
          <h3>Metadata</h3>
          <div class="field"><label>Title</label><input id="insp-title" type="text" value="${esc(e.title || "")}" /></div>
          <div class="field"><label>Creator</label><input id="insp-creator" type="text" value="${esc(e.creator || "")}" /></div>
          <div class="field"><label>Date</label><input id="insp-date" type="text" value="${esc(e.recorded_at || "")}" placeholder="YYYY-MM-DD" /></div>
          <button id="insp-save-meta" class="btn btn-secondary btn-sm" type="button" style="margin-top:6px">Save Metadata</button>
        </div>
        <div class="side-section">
          <h3>Tags</h3>
          <div id="insp-tags-wrap">${tagChips || '<span class="muted">No tags</span>'}</div>
          <div class="inline-row" style="margin-top:6px">
            <input id="insp-add-tag" type="text" list="dl-tag-suggest" placeholder="creator:Name or tag…" style="flex:1;padding:4px 6px" />
            <datalist id="dl-tag-suggest"></datalist>
            <button id="insp-btn-add-tag" class="btn btn-primary btn-sm" type="button">Add</button>
          </div>
        </div>
        <div class="side-section">
          <h3>Technical &amp; Integrity</h3>
          <div>Type: <strong>${esc(e.media_type)}</strong> · ${e.width ? `${e.width}×${e.height}` : "—"} · ${esc(e.video_codec || "—")}</div>
          <div>Size: ${fmtBytes(e.size_bytes)} · Dur: ${fmtDur(e.duration_secs)} · VR: ${esc(e.vr_projection || "No")}</div>
          <div>Integrity: <strong>${esc(e.integrity_status)}</strong></div>
          <div class="mono muted" style="font-size:10px;word-break:break-all">BLAKE3: ${esc(e.blake3_hash || "not hashed")}</div>
        </div>
        <div class="side-section">
          <h3>Relationships &amp; OCR</h3>
          ${relHtml}
          <div style="margin-top:6px">${ocrHtml}</div>
        </div>
        <div class="side-section">
          <h3>Interoperability</h3>
          <div class="inline-row" style="flex-wrap:wrap;gap:4px">
            <select id="insp-sync-policy" class="compact-sel">
              <option value="merge">Policy: Merge</option>
              <option value="manual">Policy: Manual Conflict</option>
              <option value="prefer_db">Policy: Prefer DB</option>
              <option value="prefer_external">Policy: Prefer External</option>
            </select>
            <button id="insp-sync-os" class="btn btn-ghost btn-sm" type="button">Push OS Tags</button>
            <button id="insp-pull-os" class="btn btn-ghost btn-sm" type="button">Pull OS Tags</button>
            <button id="insp-write-emb" class="btn btn-ghost btn-sm" type="button">Write Embedded</button>
            <button id="insp-exp-sidecar" class="btn btn-ghost btn-sm" type="button">Export Sidecars</button>
            <button id="insp-imp-sidecar" class="btn btn-ghost btn-sm" type="button">Import Sidecar</button>
          </div>
        </div>`;

      const dl = $("dl-tag-suggest");
      state.tags.forEach((t) => {
        const opt = document.createElement("option");
        opt.value = t.name;
        dl.appendChild(opt);
      });

      $("insp-reveal").onclick = () => invoke("reveal_output", { path: e.path });
      $("insp-open").onclick = () => invoke("open_output_player", { path: e.path });
      $("insp-verify").onclick = async () => {
        const res = await invoke("integrity_verify_files", { fileKeys: [e.file_key], fullDecode: false });
        alert(res[0] ? `${res[0].status}: ${res[0].details}` : "Checked");
        refreshLibrary();
        loadInspector(e.file_key);
      };
      $("insp-repair").onclick = () => {
        $("inp-rep-damaged").value = e.path;
        openOrganizeSubtab("integrity");
      };
      $("insp-save-meta").onclick = async () => {
        await invoke("catalog_update_asset_metadata", {
          assetId: e.asset_id,
          title: $("insp-title").value || null,
          description: null,
          creator: $("insp-creator").value || null,
          recordedAt: $("insp-date").value || null,
        });
        refreshLibrary();
      };
      $("insp-btn-add-tag").onclick = async () => {
        const val = $("insp-add-tag").value.trim();
        if (!val) return;
        await invoke("tags_assign_bulk", {
          assetIds: [e.asset_id],
          tagNames: val.split(",").map((s) => s.trim()).filter(Boolean),
          source: "manual",
          confidence: 1.0,
        });
        refreshLibrary();
        loadInspector(e.file_key);
      };
      box.querySelectorAll("[data-rm-tag]").forEach((btn) => {
        btn.onclick = async () => {
          await invoke("tags_remove_bulk", {
            assetIds: [e.asset_id],
            tagIds: [btn.dataset.rmTag],
            markRejected: true,
          });
          refreshLibrary();
          loadInspector(e.file_key);
        };
      });
      $("insp-sync-os").onclick = async () => {
        const rep = await invoke("metadata_sync_os_tags", { fileKey: e.file_key, direction: "push", policy: $("insp-sync-policy").value });
        alert(`${rep.target}: ${rep.status} — ${rep.message}`);
      };
      $("insp-pull-os").onclick = async () => {
        const rep = await invoke("metadata_sync_os_tags", { fileKey: e.file_key, direction: "pull", policy: $("insp-sync-policy").value });
        alert(`${rep.target}: ${rep.status} — ${rep.message}`);
        refreshLibrary();
        loadInspector(e.file_key);
      };
      $("insp-write-emb").onclick = async () => {
        try {
          const msg = await invoke("metadata_write_embedded", {
            fileKey: e.file_key,
            tags: (e.tags || []).map((t) => t.name),
            title: e.title,
            creator: e.creator,
          });
          alert(msg);
          refreshLibrary();
        } catch (err) { alert(err); }
      };
      $("insp-exp-sidecar").onclick = async () => {
        const files = await invoke("metadata_export_sidecars", { fileKey: e.file_key, writeJson: true, writeXmp: true });
        alert(`Exported:\n${files.join("\n")}`);
      };
      $("insp-imp-sidecar").onclick = async () => {
        const rep = await invoke("metadata_import_sidecar", { fileKey: e.file_key, policy: $("insp-sync-policy").value });
        alert(`${rep.status}: ${rep.message}`);
        refreshLibrary();
        loadInspector(e.file_key);
      };
    } catch (err) {
      box.innerHTML = `<p class="muted">${esc(err)}</p>`;
    }
  }

  function openLibraryContextMenu(x, y, item) {
    const menu = $("ctx-menu");
    const keys = Array.from(state.selectedFileKeys);
    const items = [
      { label: `Assign Tags (${keys.length})…`, fn: () => promptBulkTag() },
      { label: "Verify Integrity (BLAKE3 + Probe)", fn: () => invoke("integrity_verify_files", { fileKeys: keys, fullDecode: false }).then(() => refreshLibrary()) },
      { label: "Analyze OCR & Watermarks", fn: () => invoke("ocr_analyze_files", { fileKeys: keys }).then(() => refreshLibrary()) },
      { label: "Index Semantic Search", fn: () => invoke("semantic_index_files", { fileKeys: keys }).then((n) => alert(`Indexed ${n} frame vectors.`)) },
      { label: "Detect Black / Frozen Frames", fn: () => invoke("analyze_black_frozen", { fileKey: item.file_key }).then((r) => alert(`Sampled ${r.sampled_frames} frames · Black ratio ${(r.black_ratio * 100).toFixed(0)}% · Frozen segments: ${r.frozen_intervals.length}`)) },
      { label: "Send to Organize", fn: () => openOrganizeSubtab("batch") },
      { label: "Send to Transcode Queue", fn: () => sendSelectionToTranscode() },
      { label: "Send to Repair", fn: () => { $("inp-rep-damaged").value = item.path; openOrganizeSubtab("integrity"); } },
      { label: "Reveal in File Manager", fn: () => invoke("reveal_output", { path: item.path }) },
    ];
    menu.innerHTML = "";
    items.forEach((it) => {
      const b = document.createElement("button");
      b.type = "button";
      b.className = "ctx-item";
      b.textContent = it.label;
      b.onclick = () => { menu.classList.add("hidden"); it.fn(); };
      menu.appendChild(b);
    });
    menu.style.left = `${Math.min(x, window.innerWidth - 210)}px`;
    menu.style.top = `${Math.min(y, window.innerHeight - 280)}px`;
    menu.classList.remove("hidden");
  }
  document.addEventListener("click", () => $("ctx-menu").classList.add("hidden"));

  async function promptBulkTag() {
    const keys = Array.from(state.selectedFileKeys);
    if (!keys.length) return alert("Select one or more items in the Library first.");
    const entryMap = new Map(state.catalogPage.items.map((e) => [e.file_key, e]));
    const assetIds = keys.map((k) => entryMap.get(k)?.asset_id).filter(Boolean);
    const raw = prompt(`Assign comma-separated tags to ${assetIds.length} items:`, "status:Archived");
    if (!raw) return;
    await invoke("tags_assign_bulk", {
      assetIds,
      tagNames: raw.split(",").map((s) => s.trim()).filter(Boolean),
      source: "manual",
      confidence: 1.0,
    });
    refreshLibrary();
  }

  async function sendSelectionToTranscode() {
    const keys = Array.from(state.selectedFileKeys);
    if (!keys.length) return alert("Select video items in the Library first.");
    const snap = await invoke("get_snapshot");
    const qid = snap.active_queue_id || snap.queues[0]?.id;
    if (!qid) return;
    const n = await invoke("transcode_enqueue_from_catalog", { queueId: qid, fileKeys: keys });
    alert(`Added ${n} video(s) to Transcode list.`);
    window.dispatchEvent(new CustomEvent("ms-open-tab-encode"));
  }

  // Library Toolbar Listeners
  $("btn-lib-add-folder").onclick = async () => {
    const res = await dialog.open({ directory: true, multiple: true });
    if (!res) return;
    const dirs = Array.isArray(res) ? res : [res];
    for (const d of dirs) {
      const root = await invoke("catalog_add_root", { path: d, recursive: true, autoAnalysisLevel: 1 });
      await invoke("catalog_scan_root", { rootId: root.id, targetLevel: 1 });
    }
    refreshLibrary();
  };
  $("btn-lib-add-file").onclick = async () => {
    const res = await dialog.open({ multiple: true });
    if (!res) return;
    const files = Array.isArray(res) ? res : [res];
    for (const f of files) {
      const root = await invoke("catalog_add_root", { path: f, recursive: false, autoAnalysisLevel: 1 });
      await invoke("catalog_scan_root", { rootId: root.id, targetLevel: 1 });
    }
    refreshLibrary();
  };
  $("btn-lib-refresh-vols").onclick = async () => {
    await invoke("catalog_refresh_volumes");
    refreshLibrary();
  };
  $("inp-lib-search").oninput = (e) => {
    state.filter.search_text = e.target.value;
    state.filter.offset = 0;
    refreshLibrary();
  };
  $("chk-lib-semantic").onchange = () => refreshLibrary();
  $("sel-lib-type").onchange = (e) => { state.filter.media_type = e.target.value; state.filter.offset = 0; refreshLibrary(); };
  $("sel-lib-online").onchange = (e) => {
    state.filter.online_only = e.target.value === "online" ? true : e.target.value === "offline" ? false : null;
    state.filter.offset = 0;
    refreshLibrary();
  };
  $("btn-lib-adv-filters").onclick = () => $("lib-adv-filters-bar").classList.toggle("hidden");
  $("btn-flt-apply").onclick = () => {
    state.filter.directory_prefix = $("flt-dir").value.trim() || null;
    state.filter.codec = $("flt-codec").value.trim() || null;
    state.filter.min_width = $("flt-min-w").value ? Number($("flt-min-w").value) : null;
    state.filter.min_size_bytes = $("flt-min-mb").value ? Number($("flt-min-mb").value) * 1024 * 1024 : null;
    state.filter.min_duration = $("flt-min-dur").value ? Number($("flt-min-dur").value) : null;
    state.filter.max_duration = $("flt-max-dur").value ? Number($("flt-max-dur").value) : null;
    state.filter.include_tags = $("flt-inc-tags").value.split(",").map((s) => s.trim()).filter(Boolean);
    state.filter.exclude_tags = $("flt-exc-tags").value.split(",").map((s) => s.trim()).filter(Boolean);
    state.filter.tag_match_mode = $("flt-tag-mode").value;
    state.filter.creator = $("flt-creator").value.trim() || null;
    state.filter.ocr_query = $("flt-ocr").value.trim() || null;
    state.filter.integrity_status = $("flt-integrity").value;
    state.filter.vr_only = $("flt-vr-only").checked;
    state.filter.only_duplicates = $("flt-dup-only").checked;
    state.filter.only_untagged = $("flt-untagged-only").checked;
    state.filter.only_watermarked = $("flt-wm-only").checked;
    state.filter.offset = 0;
    localStorage.setItem("ms_lib_filter", JSON.stringify(state.filter));
    refreshLibrary();
  };
  $("btn-flt-clear").onclick = () => {
    state.activeCollectionId = null;
    state.filter = {
      ...state.filter,
      search_text: "", root_id: null, directory_prefix: null, include_tags: [], exclude_tags: [],
      codec: null, min_width: null, min_size_bytes: null, min_duration: null, max_duration: null,
      creator: null, ocr_query: null, integrity_status: null, vr_only: false,
      only_duplicates: false, only_untagged: false, only_watermarked: false, offset: 0,
    };
    localStorage.setItem("ms_lib_filter", JSON.stringify(state.filter));
    $("inp-lib-search").value = "";
    refreshLibrary();
  };
  $("btn-manage-tags").onclick = async () => {
    const action = prompt("Tag action: 1=Merge source tag into target tag, 2=Add alias to tag", "1");
    if (action === "1") {
      const src = prompt("Source tag name to merge from:", "");
      const dst = prompt("Target tag name to merge into:", "");
      if (!src || !dst) return;
      const t1 = state.tags.find((t) => t.name.toLowerCase() === src.trim().toLowerCase());
      const t2 = state.tags.find((t) => t.name.toLowerCase() === dst.trim().toLowerCase());
      if (!t1 || !t2) return alert("Both tags must exist in catalog.");
      const n = await invoke("tags_merge", { sourceTagId: t1.id, targetTagId: t2.id });
      alert(`Merged ${t1.name} into ${t2.name} (${n} asset links updated).`);
      refreshLibrary();
    } else if (action === "2") {
      const target = prompt("Existing tag name:", "");
      const alias = prompt("Alias to add:", "");
      if (!target || !alias) return;
      const t = state.tags.find((x) => x.name.toLowerCase() === target.trim().toLowerCase());
      if (!t) return alert("Tag not found.");
      await invoke("tags_add_alias", { tagId: t.id, alias });
      alert(`Added alias '${alias}' to '${t.name}'.`);
    }
  };
  $("btn-flt-save-col").onclick = async () => {
    const name = prompt("Virtual collection name:");
    if (!name) return;
    await invoke("collections_save", { id: null, name, description: null, filter: state.filter });
    refreshLibrary();
  };
  $("btn-lib-view-mode").onclick = () => {
    state.viewMode = state.viewMode === "table" ? "grid" : "table";
    $("btn-lib-view-mode").textContent = state.viewMode === "table" ? "Grid" : "Table";
    renderLibraryItems();
  };
  $("btn-lib-columns").onclick = () => {
    const all = ["filename", "media_type", "resolution", "codec", "duration", "size", "bitrate", "integrity", "tags", "path"];
    const next = prompt(`Visible columns (comma-separated from: ${all.join(", ")}):`, state.columns.join(","));
    if (!next) return;
    state.columns = next.split(",").map((s) => s.trim()).filter((c) => all.includes(c));
    localStorage.setItem("ms_lib_cols", JSON.stringify(state.columns));
    renderLibraryItems();
  };
  $("btn-lib-bulk-tag").onclick = promptBulkTag;
  $("btn-lib-send-organize").onclick = () => openOrganizeSubtab("batch");
  $("btn-lib-send-transcode").onclick = sendSelectionToTranscode;
  $("btn-lib-analyze-menu").onclick = async () => {
    const keys = Array.from(state.selectedFileKeys);
    if (!keys.length) return alert("Select files in Library first.");
    const choice = prompt("Enter analysis action: 1=OCR+Watermarks, 2=Semantic Embeddings, 3=Integrity Verify", "1");
    if (choice === "1") await invoke("ocr_analyze_files", { fileKeys: keys });
    if (choice === "2") await invoke("semantic_index_files", { fileKeys: keys });
    if (choice === "3") await invoke("integrity_verify_files", { fileKeys: keys, fullDecode: false });
    refreshLibrary();
  };
  $("btn-lib-select-all").onclick = () => {
    state.catalogPage.items.forEach((it) => state.selectedFileKeys.add(it.file_key));
    renderLibraryItems();
    updateSelectionPill();
  };
  $("btn-lib-prev").onclick = () => {
    state.filter.offset = Math.max(0, state.filter.offset - state.filter.limit);
    refreshLibrary();
  };
  $("btn-lib-next").onclick = () => {
    if (state.filter.offset + state.filter.limit < state.catalogPage.total_count) {
      state.filter.offset += state.filter.limit;
      refreshLibrary();
    }
  };

  // Keyboard shortcuts
  document.addEventListener("keydown", (ev) => {
    if (ev.ctrlKey && ev.key.toLowerCase() === "f" && state.currentView === "library") {
      ev.preventDefault();
      $("inp-lib-search").focus();
    } else if (ev.ctrlKey && ev.key.toLowerCase() === "a" && state.currentView === "library" && document.activeElement.tagName !== "INPUT") {
      ev.preventDefault();
      state.catalogPage.items.forEach((it) => state.selectedFileKeys.add(it.file_key));
      renderLibraryItems();
      updateSelectionPill();
    }
  });

  // =========================================================================
  // DUPLICATES VIEW
  // =========================================================================

  $("btn-dup-scan").onclick = async () => {
    await invoke("duplicates_run_scan", {
      alsoSha256: $("chk-dup-sha256").checked,
      includePerceptual: $("chk-dup-perceptual").checked,
    });
    alert("Duplicate & similarity scan started in Tasks.");
    refreshDuplicates();
  };
  $("sel-dup-filter").onchange = () => refreshDuplicates();

  async function refreshDuplicates() {
    const kind = $("sel-dup-filter").value;
    state.dupGroups = await invoke("duplicates_list_groups", { matchKind: kind === "all" ? null : kind });
    const list = $("dup-groups-list");
    list.innerHTML = "";
    let totalSave = 0;
    state.dupGroups.forEach((g) => {
      totalSave += g.estimated_savings_bytes;
      const card = document.createElement("div");
      card.className = "dup-group-card" + (state.selectedDupGroupId === g.group_id ? " active" : "");
      card.innerHTML = `
        <div><strong>${esc(g.match_kind)}</strong> · ${(g.confidence * 100).toFixed(0)}%</div>
        <div class="muted">${g.members.length} files · Save ${fmtBytes(g.estimated_savings_bytes)}</div>`;
      card.onclick = () => {
        state.selectedDupGroupId = g.group_id;
        refreshDuplicates();
      };
      list.appendChild(card);
    });
    $("lbl-dup-summary").textContent = `${state.dupGroups.length} groups · Est. savings ${fmtBytes(totalSave)}`;
    renderDupDetail();
  }

  async function renderDupDetail() {
    const pane = $("dup-detail-pane");
    const grp = state.dupGroups.find((g) => g.group_id === state.selectedDupGroupId) || state.dupGroups[0];
    if (!grp) {
      pane.innerHTML = `<p class="muted">No duplicate groups found for the selected filter.</p>`;
      return;
    }
    const kept = grp.members.find((m) => m.suggested_keep) || grp.members[0];
    const redundant = grp.members.filter((m) => m.entry.file_key !== kept.entry.file_key);
    const edgeInfo = grp.edges
      .map((ed) => {
        if (ed.overlap_start_a !== null && ed.overlap_start_a !== undefined) {
          return `<div>Overlap: A [${fmtDur(ed.overlap_start_a)}–${fmtDur(ed.overlap_end_a)}] (${(ed.overlap_pct_a || 0).toFixed(0)}%) ↔ B [${fmtDur(ed.overlap_start_b)}–${fmtDur(ed.overlap_end_b)}] (${(ed.overlap_pct_b || 0).toFixed(0)}%)</div>`;
        }
        return `<div>${esc(ed.match_kind)} · confidence ${(ed.confidence * 100).toFixed(1)}%</div>`;
      })
      .join("");

    pane.innerHTML = `
      <div class="card-panel">
        <div class="inline-row" style="justify-content:space-between">
          <div><strong>Group (${esc(grp.match_kind)})</strong> · Estimated savings: <strong>${fmtBytes(grp.estimated_savings_bytes)}</strong></div>
          <div class="inline-row">
            <button id="dup-merge-tags" class="btn btn-secondary btn-sm" type="button">Merge Tags into Kept</button>
            <button id="dup-dismiss" class="btn btn-ghost btn-sm" type="button">Dismiss Group</button>
            <button id="dup-trash-checked" class="btn btn-danger-outline btn-sm" type="button">Move Checked Copies to Trash</button>
          </div>
        </div>
        <div class="muted" style="margin-top:6px">${edgeInfo}</div>
      <div class="muted" style="margin-top:4px;font-size:12px;">${
        grp.match_kind === "exact"
          ? "Exact byte-for-byte duplicate (BLAKE3 verified). At least one copy is always protected from deletion."
          : "Visual / stream similarity match — audio tracks and endings may differ. Nothing is pre-checked; compare side-by-side before selecting."
      }</div>
      <div class="compare-grid" id="dup-compare-frames" style="margin:10px 0"></div>
      <table class="table-simple">
        <thead><tr><th>Trash?</th><th>Path</th><th>Size</th><th>Res / Codec</th><th>Duration / Bitrate</th><th>Integrity</th><th>Recommendation</th></tr></thead>
        <tbody>
          ${grp.members
            .map(
              (m) => `<tr>
                <td><input type="checkbox" class="dup-copy-chk" value="${esc(m.entry.file_key)}" ${grp.match_kind === "exact" && !m.suggested_keep ? "checked" : ""} /></td>
                <td class="mono" style="max-width:260px;word-break:break-all">${esc(m.entry.path)}</td>
                <td>${fmtBytes(m.entry.size_bytes)}</td>
                <td>${m.entry.width ? `${m.entry.width}×${m.entry.height}` : "—"} · ${esc(m.entry.video_codec || "—")}</td>
                <td>${fmtDur(m.entry.duration_secs)} · ${m.entry.bitrate ? `${Math.round(m.entry.bitrate / 1000)}k` : "—"}</td>
                <td>${esc(m.entry.integrity_status)}</td>
                <td>${m.suggested_keep ? '<span class="badge badge-ok">Keep</span> ' : ""}${esc(m.keep_reason)}</td>
              </tr>`
            )
            .join("")}
        </tbody>
      </table>`;

    // Side-by-side frame preview + synchronized timestamp scrubber for top 2 members
    const compDiv = $("dup-compare-frames");
    const pair = grp.members.slice(0, 2);
    const maxDur = Math.max(1, ...pair.map((m) => m.entry.duration_secs || 1));
    const syncBar = document.createElement("div");
    syncBar.style.gridColumn = "1 / -1";
    syncBar.className = "inline-row";
    syncBar.innerHTML = `<label>Synchronized Timestamp: <strong id="dup-ts-lbl">0.5s</strong></label>
      <input id="dup-sync-ts" type="range" min="0" max="${maxDur.toFixed(1)}" step="0.5" value="0.5" style="flex:1" />
      <button id="btn-dup-sync-load" class="btn btn-secondary btn-sm" type="button">Compare Frames at Timestamp</button>`;
    compDiv.appendChild(syncBar);

    const frameImgs = [];
    for (const m of pair) {
      const card = document.createElement("div");
      card.className = "compare-pane";
      const turl = thumbUrlFromAbs(m.entry.thumbnail_path);
      const audioLbl = m.entry.audio_codec ? `Audio: ${esc(m.entry.audio_codec)}` : "No audio";
      card.innerHTML = `<div class="compare-label">${esc(m.entry.filename)} (${fmtBytes(m.entry.size_bytes)} · ${audioLbl})</div>${
        turl ? `<img src="${turl}" />` : `<div class="thumb-placeholder">Click Compare Frames to decode frame</div>`
      }`;
      compDiv.appendChild(card);
      frameImgs.push({ member: m, card });
    }
    $("dup-sync-ts").oninput = (e) => { $("dup-ts-lbl").textContent = `${Number(e.target.value).toFixed(1)}s`; };
    $("btn-dup-sync-load").onclick = async () => {
      const ts = Number($("dup-sync-ts").value);
      for (const item of frameImgs) {
        try {
          const bytes = await invoke("get_comparison_frame", {
            mediaPath: item.member.entry.path,
            timestampSecs: ts,
            fullResolution: false,
          });
          const url = URL.createObjectURL(new Blob([new Uint8Array(bytes)], { type: "image/jpeg" }));
          let img = item.card.querySelector("img");
          if (!img) {
            const ph = item.card.querySelector(".thumb-placeholder");
            if (ph) ph.remove();
            img = document.createElement("img");
            item.card.appendChild(img);
          }
          img.src = url;
        } catch (_) {}
      }
    };

    $("dup-merge-tags").onclick = async () => {
      const fromIds = redundant.map((r) => r.entry.asset_id);
      const n = await invoke("duplicates_merge_tags", { fromAssetIds: fromIds, intoAssetId: kept.entry.asset_id });
      alert(`Merged ${n} tag assignment(s) into ${kept.entry.filename}.`);
    };
    $("dup-dismiss").onclick = async () => {
      for (const ed of grp.edges) await invoke("duplicates_dismiss_edge", { edgeId: ed.id });
      refreshDuplicates();
    };
    $("dup-trash-checked").onclick = async () => {
      const keys = Array.from(pane.querySelectorAll(".dup-copy-chk:checked")).map((c) => c.value);
      if (!keys.length) return;
      try {
        const plan = await invoke("preview_tab_trash", {
          queueId: null,
          itemIds: keys,
          removalReason: "library_duplicate_copy",
        });
        const listLines = (plan.items || [])
          .map((it) => `• ${it.filename} (${it.parent_dir}) — ${fmtBytes(it.size_bytes)} [${it.eligible ? "Ready" : `BLOCKED: ${it.reason || "Protected"}`}]`)
          .join("\n");
        const supportWarn = !plan.trash_supported && plan.trash_support_error
          ? `\nWARNING — OS Trash unavailable: ${plan.trash_support_error}\n`
          : "";
        if (plan.eligible_count === 0) {
          alert(`No checked files can be moved to OS Trash.${supportWarn}\n${listLines}`);
          return;
        }
        const ok = confirm(
          `Scope: Library Duplicate Group (${grp.match_kind})\nMove ${plan.eligible_count} file(s) (${fmtBytes(plan.total_bytes)}) to recoverable OS Trash?${plan.blocked_count ? ` (${plan.blocked_count} blocked/protected will be kept)` : ""}${supportWarn}\n\n${listLines}`
        );
        if (!ok) return;
        const res = await invoke("execute_tab_trash_plan", { planId: plan.plan_id });
        const errs = (res.outcomes || []).filter((o) => !o.moved).map((o) => `${o.filename}: ${o.error || "Kept"}`);
        if (errs.length) {
          alert(`Moved ${res.moved_count} to OS Trash; ${res.failed_count} kept:\n${errs.join("\n")}`);
        }
        refreshDuplicates();
      } catch (err) {
        alert(String(err));
      }
    };
  }

  // =========================================================================
  // ORGANIZE VIEW (Rename, Move/Copy, Local LLM, History/Undo)
  // =========================================================================

  function invalidateOrgPreview() {
    state.orgPreview = null;
    $("btn-org-execute").disabled = true;
    const w = $("org-preview-wrap");
    if (w && state.orgMode !== "history") w.innerHTML = "";
  }

  $("btn-org-mode-rename").onclick = () => { state.orgMode = "rename"; state.orgPreview = null; renderOrganizeView(); };
  $("btn-org-mode-move").onclick = () => { state.orgMode = "move"; state.orgPreview = null; renderOrganizeView(); };
  $("btn-org-mode-llm").onclick = () => { state.orgMode = "llm"; state.orgPreview = null; renderOrganizeView(); };
  $("btn-org-mode-history").onclick = () => { state.orgMode = "history"; state.orgPreview = null; renderOrganizeView(); };

  async function renderOrganizeView() {
    updateSelectionPill();
    const panel = $("org-controls-panel");
    const prevWrap = $("org-preview-wrap");
    $("btn-org-execute").disabled = !state.orgPreview || state.orgPreview.valid_count === 0;

    if (state.orgMode === "rename") {
      panel.innerHTML = `
        <div class="compact-filter-row">
          <label>Template <input id="org-ren-tmpl" type="text" value="{creator}_{date}_{title}_{resolution}_{index}.{extension}" style="width:280px" /></label>
          <label>Prefix <input id="org-ren-pfx" type="text" style="width:70px" /></label>
          <label>Suffix <input id="org-ren-sfx" type="text" style="width:70px" /></label>
          <label>Remove <input id="org-ren-rem" type="text" style="width:70px" /></label>
          <label>Find <input id="org-ren-find" type="text" style="width:85px" /></label>
          <label>Replace <input id="org-ren-rep" type="text" style="width:85px" /></label>
          <label class="toggle-inline"><input type="checkbox" id="org-ren-regex" /><span>Regex</span></label>
          <label>Start# <input id="org-ren-start" type="number" value="1" style="width:52px" /></label>
          <label>Pad <input id="org-ren-pad" type="number" value="3" style="width:46px" /></label>
          <label>Case <select id="org-ren-case"><option value="preserve">Preserve</option><option value="lower">lower</option><option value="upper">UPPER</option><option value="title">Title</option></select></label>
          <label class="toggle-inline"><input type="checkbox" id="org-ren-uni" /><span>Unicode norm</span></label>
        </div>`;
      panel.querySelectorAll("input, select").forEach((inp) => {
        inp.addEventListener("input", invalidateOrgPreview);
        inp.addEventListener("change", invalidateOrgPreview);
      });
    } else if (state.orgMode === "move") {
      panel.innerHTML = `
        <div class="compact-filter-row">
          <button id="btn-org-pick-dest" class="btn btn-secondary btn-sm" type="button">Dest Folder…</button>
          <input id="org-mv-dest" type="text" placeholder="Destination root directory…" style="width:240px" />
          <label>Folder Template <input id="org-mv-tmpl" type="text" value="Media/{creator}/{year}/{title}.{extension}" style="width:260px" /></label>
          <label>Op <select id="org-mv-copy"><option value="move">Move (copy+verify across drives)</option><option value="copy">Copy / Export</option></select></label>
          <label>Multi-tag <select id="org-mv-multi"><option value="first">First value (no silent dup)</option><option value="join">Join with _</option></select></label>
        </div>`;
      panel.querySelectorAll("input, select").forEach((inp) => {
        inp.addEventListener("input", invalidateOrgPreview);
        inp.addEventListener("change", invalidateOrgPreview);
      });
      $("btn-org-pick-dest").onclick = async () => {
        const d = await dialog.open({ directory: true });
        if (d) {
          $("org-mv-dest").value = d;
          invalidateOrgPreview();
        }
      };
    } else if (state.orgMode === "llm") {
      panel.innerHTML = `
        <div class="compact-filter-row">
          <input id="org-llm-instr" type="text" placeholder="e.g. Rename selected videos using detected titles and group by creator…" style="flex:1;min-width:340px" />
          <button id="btn-org-llm-ask" class="btn btn-primary btn-sm" type="button">Generate Previewed Proposal</button>
        </div>
        <div id="org-llm-tags-box" class="muted" style="margin-top:6px"></div>`;
      $("btn-org-llm-ask").onclick = async () => {
        const keys = Array.from(state.selectedFileKeys);
        if (!keys.length) return alert("Select files in Library first.");
        try {
          const prop = await invoke("llm_propose_organization", { instruction: $("org-llm-instr").value, fileKeys: keys });
          const tagsBox = $("org-llm-tags-box");
          if (prop.suggested_tags && prop.suggested_tags.length) {
            tagsBox.innerHTML = `Suggested tags for ${prop.suggested_tags.length} item(s). <button id="btn-llm-apply-tags" class="btn btn-secondary btn-sm" type="button">Apply Suggested Tags</button>`;
            $("btn-llm-apply-tags").onclick = async () => {
              const entryMap = new Map(state.catalogPage.items.map((e) => [e.file_key, e]));
              for (const [fk, tnames] of prop.suggested_tags) {
                const e = entryMap.get(fk);
                if (e) await invoke("tags_assign_bulk", { assetIds: [e.asset_id], tagNames: tnames, source: "ai_suggested", confidence: 0.85 });
              }
              alert("Applied AI-suggested tags (manual tags remain protected).");
              refreshLibrary();
            };
          }
          if (prop.rename_preview && prop.move_preview) {
            const useMove = confirm("Proposal includes both Rename and Move previews. Click OK to preview Move, or Cancel to preview Rename.");
            state.orgPreview = useMove ? prop.move_preview : prop.rename_preview;
          } else {
            state.orgPreview = prop.rename_preview || prop.move_preview;
          }
          renderOrgPreviewTable(prop.summary);
        } catch (err) { alert(err); }
      };
    } else if (state.orgMode === "history") {
      panel.innerHTML = `<div class="muted">Durable operation journal (supports safe Undo when original paths remain free).</div>`;
      const hist = await invoke("organize_list_history");
      prevWrap.innerHTML = `<table class="table-simple">
        <thead><tr><th>Date</th><th>Summary</th><th>Items</th><th>State</th><th>Action</th></tr></thead>
        <tbody>${hist.map((h) => `<tr>
          <td>${new Date(h.created_at * 1000).toLocaleString()}</td>
          <td>${esc(h.summary)}</td>
          <td>${h.entries.length}</td>
          <td>${esc(h.state)}</td>
          <td>${h.state !== "undone" ? `<button class="btn btn-ghost btn-sm" data-undo="${esc(h.id)}">Undo</button>` : "Undone"}</td>
        </tr>`).join("")}</tbody></table>`;
      prevWrap.querySelectorAll("[data-undo]").forEach((b) => {
        b.onclick = async () => {
          try {
            const res = await invoke("organize_undo", { operationId: b.dataset.undo });
            alert(`Undid ${res.succeeded} item(s).`);
            renderOrganizeView();
            refreshLibrary();
          } catch (err) { alert(err); }
        };
      });
    }
  }

  $("btn-org-preview").onclick = async () => {
    const keys = Array.from(state.selectedFileKeys);
    if (!keys.length) return alert("Select files in Library first.");
    try {
      if (state.orgMode === "rename") {
        state.orgPreview = await invoke("organize_preview_rename", {
          fileKeys: keys,
          options: {
            template: $("org-ren-tmpl").value,
            prefix: $("org-ren-pfx").value || null,
            suffix: $("org-ren-sfx").value || null,
            remove_text: $("org-ren-rem").value || null,
            find_text: $("org-ren-find").value || null,
            replace_text: $("org-ren-rep").value || null,
            use_regex: $("org-ren-regex").checked,
            start_index: Number($("org-ren-start").value) || 1,
            index_pad: Number($("org-ren-pad").value) || 3,
            case_mode: $("org-ren-case").value,
            normalize_unicode: $("org-ren-uni").checked,
          },
        });
      } else if (state.orgMode === "move") {
        const dest = $("org-mv-dest").value.trim();
        if (!dest) return alert("Choose a destination directory.");
        state.orgPreview = await invoke("organize_preview_move_copy", {
          fileKeys: keys,
          destRoot: dest,
          folderTemplate: $("org-mv-tmpl").value || null,
          isCopy: $("org-mv-copy").value === "copy",
          multiValuePolicy: $("org-mv-multi").value,
        });
      }
      renderOrgPreviewTable();
    } catch (err) { alert(err); }
  };

  function renderOrgPreviewTable(note) {
    const p = state.orgPreview;
    if (!p) return;
    $("btn-org-execute").disabled = p.valid_count === 0 || p.error_count > 0;
    $("org-preview-wrap").innerHTML = `
      <div style="margin-bottom:8px">${note ? `<strong>${esc(note)}</strong> · ` : ""}Valid: ${p.valid_count} · Collisions: ${p.collision_count} · Errors: ${p.error_count}</div>
      <table class="table-simple">
        <thead><tr><th>Op</th><th>Current Path</th><th>New Path</th><th>Cross-Vol</th><th>Status</th></tr></thead>
        <tbody>${p.items.map((it) => `<tr>
          <td>${esc(it.op_kind)}</td>
          <td class="mono">${esc(it.old_path)}</td>
          <td class="mono">${esc(it.new_path)}</td>
          <td>${it.cross_volume ? "Yes (copy+verify)" : "Same vol"}</td>
          <td>${it.validation_error ? `<span class="badge badge-err">${esc(it.validation_error)}</span>` : '<span class="badge badge-ok">Ready</span>'}</td>
        </tr>`).join("")}</tbody>
      </table>`;
  }

  $("btn-org-execute").onclick = async () => {
    if (!state.orgPreview) return;
    try {
      const res = await invoke("organize_execute_batch", {
        items: state.orgPreview.plan_id ? null : state.orgPreview.items,
        summary: `${state.orgMode} (${state.orgPreview.items.length} items)`,
        planId: state.orgPreview.plan_id || null,
      });
      const errNote = res.errors && res.errors.length ? `\n\nNotes:\n${res.errors.join("\n")}` : "";
      alert(`Executed: ${res.succeeded} succeeded, ${res.failed} failed.${errNote}`);
      state.orgPreview = null;
      refreshLibrary();
      renderOrganizeView();
    } catch (err) { alert(err); }
  };

  // =========================================================================
  // PEOPLE VIEW (Opt-in Face Grouping)
  // =========================================================================

  $("btn-people-refresh").onclick = refreshPeopleView;
  $("btn-people-scan-selected").onclick = async () => {
    const keys = Array.from(state.selectedFileKeys);
    if (!keys.length) return alert("Select files in Library first.");
    try {
      const n = await invoke("faces_run_grouping", { fileKeys: keys });
      alert(`Detected ${n} face observation(s).`);
      refreshPeopleView();
    } catch (err) { alert(err); }
  };
  $("btn-people-purge").onclick = async () => {
    if (!confirm("Permanently delete all locally stored face crops, embeddings, and person groups?")) return;
    const n = await invoke("faces_purge_all_biometrics");
    alert(`Deleted ${n} biometric observations.`);
    refreshPeopleView();
  };

  async function refreshPeopleView() {
    state.faceGroups = await invoke("faces_list_groups");
    const list = $("people-groups-list");
    list.innerHTML = "";
    state.faceGroups.forEach((g) => {
      const card = document.createElement("div");
      card.className = "dup-group-card" + (state.selectedFaceGroupId === g.id ? " active" : "");
      card.innerHTML = `<strong>${esc(g.label || (g.is_unassigned ? "Unassigned" : "Unnamed Person"))}</strong><div class="muted">${g.observation_count} observations across ${g.media_count} files</div>`;
      card.onclick = () => { state.selectedFaceGroupId = g.id; refreshPeopleView(); };
      list.appendChild(card);
    });
    const detail = $("people-detail-pane");
    const sel = state.faceGroups.find((g) => g.id === state.selectedFaceGroupId) || state.faceGroups[0];
    if (!sel) { detail.innerHTML = `<p class="muted">No face groups yet.</p>`; return; }
    const obs = await invoke("faces_list_observations", { groupId: sel.id });
    detail.innerHTML = `
      <div class="card-panel">
        <div class="inline-row">
          <input id="inp-face-label" type="text" value="${esc(sel.label || "")}" placeholder="Label person…" />
          <button id="btn-face-label" class="btn btn-primary btn-sm" type="button">Save Label</button>
          <button id="btn-face-merge" class="btn btn-secondary btn-sm" type="button">Merge into Group…</button>
        </div>
      </div>
      <div class="lib-grid-wrap">${obs.map((o) => {
        const url = thumbUrlFromAbs(o.crop_path);
        return `<div class="thumb-card">${url ? `<img src="${url}" />` : '<div class="thumb-placeholder">Face</div>'}
          <div class="thumb-meta">
            <div class="thumb-title">${fmtDur(o.timestamp_secs)}</div>
            <div class="inline-row">
              <button class="btn btn-ghost btn-sm" data-split="${esc(o.id)}">Split</button>
              <button class="btn btn-ghost btn-sm" data-excl="${esc(o.id)}">Exclude</button>
            </div>
          </div></div>`;
      }).join("")}</div>`;
    $("btn-face-label").onclick = async () => {
      await invoke("faces_label_group", { groupId: sel.id, label: $("inp-face-label").value });
      refreshPeopleView();
    };
    $("btn-face-merge").onclick = async () => {
      const target = prompt("Target group ID to merge into:", state.faceGroups.find((x) => x.id !== sel.id)?.id || "");
      if (target) {
        await invoke("faces_merge_groups", { sourceGroupId: sel.id, targetGroupId: target });
        refreshPeopleView();
      }
    };
    detail.querySelectorAll("[data-split]").forEach((b) => {
      b.onclick = () => invoke("faces_split_observation", { observationId: b.dataset.split }).then(refreshPeopleView);
    });
    detail.querySelectorAll("[data-excl]").forEach((b) => {
      b.onclick = () => invoke("faces_exclude_observation", { observationId: b.dataset.excl }).then(refreshPeopleView);
    });
  }

  // =========================================================================
  // REPAIR & INTEGRITY VIEW
  // =========================================================================

  $("btn-rep-pick-damaged").onclick = async () => {
    const p = await dialog.open({ multiple: false });
    if (p) $("inp-rep-damaged").value = p;
  };
  $("btn-rep-pick-ref").onclick = async () => {
    const p = await dialog.open({ multiple: false });
    if (p) $("inp-rep-ref").value = p;
  };
  $("btn-rep-diagnose").onclick = async () => {
    const path = $("inp-rep-damaged").value.trim();
    if (!path) return alert("Select a damaged video file first.");
    try {
      const d = await invoke("repair_diagnose", { path, fileKey: state.activeFileKey });
      state.repairDiagnosis = d;
      $("repair-result-panel").innerHTML = `
        <h3>Diagnosis: ${esc(d.container_integrity)}</h3>
        <p>${esc(d.summary)}</p>
        <div>FFprobe readable: <strong>${d.ffprobe_readable}</strong> · Missing moov: <strong>${d.missing_moov_atom}</strong> · Truncated mdat: <strong>${d.truncated_mdat}</strong> · Decoder errors: <strong>${d.decoder_errors}</strong></div>
        <div style="margin-top:8px">${d.atoms.map((a) => `<span class="atom-pill ${a.truncated ? "bad" : ""}">${esc(a.box_type)} (${fmtBytes(a.size)})</span>`).join("")}</div>`;
      refreshRepairView();
    } catch (err) { alert(err); }
  };
  $("btn-rep-run").onclick = async () => {
    const damagedPath = $("inp-rep-damaged").value.trim();
    if (!damagedPath) return alert("Select a damaged video file first.");
    try {
      const res = await invoke("repair_execute", {
        damagedPath,
        referencePath: $("inp-rep-ref").value.trim() || null,
        outputDir: null,
        sourceFileKey: state.activeFileKey,
      });
      $("repair-result-panel").innerHTML = `
        <h3>Recovery Succeeded (${esc(res.strategy_used)})</h3>
        <div>Output: <span class="mono">${esc(res.output_path)}</span> (${fmtBytes(res.output_bytes)} · ${fmtDur(res.duration_secs)})</div>
        <div>Verified decoded: <strong>${res.verified_decoded}</strong> · Partial recovery: <strong>${res.partial_recovery}</strong></div>
        <pre class="mono" style="white-space:pre-wrap;margin-top:8px">${esc(res.logs.join("\n"))}</pre>`;
      refreshLibrary();
      refreshRepairView();
    } catch (err) { alert(err); }
  };
  $("btn-int-par2").onclick = async () => {
    const target = $("inp-rep-damaged").value.trim();
    if (!target) return alert("Select a file path first.");
    try {
      const msg = await invoke("integrity_create_par2", { targetPath: target, redundancyPct: 10 });
      alert(msg);
    } catch (err) { alert(err); }
  };
  $("btn-int-export").onclick = async () => {
    const dest = await dialog.save({ defaultPath: "mediashack-checksums.json" });
    if (!dest) return;
    const fmt = dest.endsWith(".sha256") ? "sha256sum" : "json";
    const cnt = await invoke("integrity_export_manifest", { destPath: dest, format: fmt, fileKeys: null });
    alert(`Exported ${cnt} checksum entries to ${dest}.`);
  };

  async function refreshRepairView() {
    const hist = await invoke("integrity_list_history", { limit: 50 });
    $("integrity-history-panel").innerHTML = `
      <h3>Archival Integrity History</h3>
      <table class="table-simple">
        <thead><tr><th>Checked</th><th>Path</th><th>Check</th><th>Status</th><th>Copies</th><th>Details</th></tr></thead>
        <tbody>${hist.map((h) => `<tr>
          <td>${new Date(h.checked_at * 1000).toLocaleString()}</td>
          <td class="mono">${esc(h.path)}</td>
          <td>${esc(h.check_type)}</td>
          <td><strong>${esc(h.status)}</strong></td>
          <td>${h.backup_copy_count}${h.backup_warning ? " ⚠" : ""}</td>
          <td>${esc(h.details || "")}</td>
        </tr>`).join("")}</tbody>
      </table>`;
  }

  // =========================================================================
  // TASKS & RESOURCE BUDGETS VIEW
  // =========================================================================

  $("btn-tasks-refresh").onclick = refreshTasksView;
  $("btn-cache-evict").onclick = async () => {
    const stats = await invoke("cache_get_stats");
    const freed = await invoke("cache_evict", { maxBytes: stats.budget_bytes });
    alert(`Evicted ${fmtBytes(freed)} from analysis cache.`);
    refreshTasksView();
  };
  $("btn-cache-clear").onclick = async () => {
    if (!confirm("Clear all cached thumbnails and extracted frames?")) return;
    const freed = await invoke("cache_clear");
    alert(`Cleared ${fmtBytes(freed)}.`);
    refreshTasksView();
  };

  async function refreshTasksView() {
    const [tasks, budgets, cache] = await Promise.all([
      invoke("tasks_list"),
      invoke("tasks_get_budgets"),
      invoke("cache_get_stats"),
    ]);
    state.tasks = tasks;
    state.budgets = budgets;
    const activeCnt = tasks.filter((t) => t.state === "running" || t.state === "queued").length;
    window.dispatchEvent(new CustomEvent("ms-tasks-count-changed", { detail: { count: activeCnt } }));

    $("tasks-workspace").innerHTML = `
      <div class="card-panel">
        <h3>Resource Budgets &amp; Cache (${fmtBytes(cache.total_bytes)} / ${fmtBytes(cache.budget_bytes)}, ${cache.file_count} files)</h3>
        <div class="compact-filter-row">
          <label>CPU Workers <input id="b-cpu" type="number" value="${budgets.cpu_workers}" style="width:60px" /></label>
          <label>GPU Slots <input id="b-gpu" type="number" value="${budgets.gpu_concurrency}" style="width:60px" /></label>
          <label>Disk I/O <input id="b-io" type="number" value="${budgets.disk_io_concurrency}" style="width:60px" /></label>
          <label>Max FFmpeg <input id="b-ff" type="number" value="${budgets.max_ffmpeg_processes}" style="width:60px" /></label>
          <label>Memory MB <input id="b-mem" type="number" value="${budgets.memory_limit_mb}" style="width:80px" /></label>
          <label>Cache MB <input id="b-cache" type="number" value="${budgets.cache_max_mb}" style="width:80px" /></label>
          <label class="toggle-inline"><input id="b-throttle" type="checkbox" ${budgets.throttle_analysis_while_transcoding ? "checked" : ""} /><span>Throttle analysis while transcoding</span></label>
          <button id="btn-save-budgets" class="btn btn-primary btn-sm" type="button">Save Budgets</button>
        </div>
      </div>
      <div class="card-panel">
        <h3>Background Tasks</h3>
        <table class="table-simple">
          <thead><tr><th>Task</th><th>Category</th><th>Priority</th><th>State</th><th>Progress / Throughput</th><th>Controls</th></tr></thead>
          <tbody>${tasks.map((t) => `<tr>
            <td>${esc(t.task_type)}</td>
            <td>${esc(t.category)} (${esc(t.resource_kind)})</td>
            <td>${t.priority}</td>
            <td><strong>${esc(t.state)}</strong>${t.error_message ? `<div class="muted">${esc(t.error_message)}</div>` : ""}</td>
            <td>${Math.round(t.progress_pct)}% ${t.throughput_label ? `· ${esc(t.throughput_label)}` : ""}</td>
            <td>
              <button class="btn btn-ghost btn-sm" data-tpause="${esc(t.id)}">Pause</button>
              <button class="btn btn-ghost btn-sm" data-tresume="${esc(t.id)}">Resume</button>
              <button class="btn btn-ghost btn-sm" data-tcancel="${esc(t.id)}">Cancel</button>
            </td>
          </tr>`).join("")}</tbody>
        </table>
      </div>`;

    $("btn-save-budgets").onclick = async () => {
      await invoke("tasks_save_budgets", {
        budgets: {
          cpu_workers: Number($("b-cpu").value),
          gpu_concurrency: Number($("b-gpu").value),
          memory_limit_mb: Number($("b-mem").value),
          disk_io_concurrency: Number($("b-io").value),
          max_ffmpeg_processes: Number($("b-ff").value),
          cache_max_mb: Number($("b-cache").value),
          throttle_analysis_while_transcoding: $("b-throttle").checked,
        },
      });
      alert("Saved resource budgets.");
    };
    $("tasks-workspace").querySelectorAll("[data-tpause]").forEach((b) => { b.onclick = () => invoke("tasks_pause", { taskId: b.dataset.tpause }).then(refreshTasksView); });
    $("tasks-workspace").querySelectorAll("[data-tresume]").forEach((b) => { b.onclick = () => invoke("tasks_resume", { taskId: b.dataset.tresume }).then(refreshTasksView); });
    $("tasks-workspace").querySelectorAll("[data-tcancel]").forEach((b) => { b.onclick = () => invoke("tasks_cancel", { taskId: b.dataset.tcancel }).then(refreshTasksView); });
  }

  // =========================================================================
  // SETTINGS, OCR REVIEW & DATABASE BACKUP VIEW
  // =========================================================================

  function syncUpdatePill() {
    const pill = $("btn-update-pill");
    const lbl = $("lbl-update-pill");
    if (!pill || !lbl) return;
    const upd = state.updateCheck;
    if (upd && upd.update_available) {
      lbl.textContent = `Update ${upd.tag_name || `v${upd.latest_version}`}`;
      pill.classList.remove("hidden");
    } else {
      pill.classList.add("hidden");
    }
  }

  async function runUpdateCheck({ silent = false } = {}) {
    if (state.updateChecking) return;
    state.updateChecking = true;
    state.updateError = null;
    if (!silent && state.currentView === "settings") refreshSettingsCardUpdatesOnly();
    try {
      const res = await invoke("check_for_updates");
      state.updateCheck = res;
      syncUpdatePill();
    } catch (e) {
      if (!silent) state.updateError = String(e);
    } finally {
      state.updateChecking = false;
      if (state.currentView === "settings") refreshSettingsCardUpdatesOnly();
    }
  }

  function renderUpdatesCardHtml() {
    const upd = state.updateCheck;
    const prog = state.updateProgress;
    const curVer = upd?.current_version || "0.3.6";
    const pct = prog?.percent != null ? Math.max(0, Math.min(100, prog.percent)) : null;

    let statusLine = `<span class="muted">Check GitHub Releases for newer builds.</span>`;
    if (state.updateChecking) {
      statusLine = `<span>Checking GitHub Releases…</span>`;
    } else if (state.updateError) {
      statusLine = `<span style="color:var(--err);">${esc(state.updateError)}</span>`;
    } else if (state.updateStatusMessage) {
      statusLine = `<span style="color:var(--ok);font-weight:600;">${esc(state.updateStatusMessage)}</span>`;
    } else if (upd && upd.update_available) {
      statusLine = `<span style="color:var(--ok);font-weight:600;">Update available: ${esc(upd.tag_name)}</span>` +
        (upd.asset_size ? ` <span class="muted">(${fmtBytes(upd.asset_size)} · ${esc(upd.platform_label)})</span>` : "");
    } else if (upd) {
      statusLine = `<span style="color:var(--ok);">You are up to date (v${esc(curVer)}).</span>`;
    }

    const downloadBtn = upd && upd.update_available && upd.asset_url && upd.asset_name
      ? `<button id="btn-download-update" class="btn btn-primary btn-sm" type="button" ${state.updateDownloading ? "disabled" : ""}>${state.updateDownloading ? "Updating…" : "Download, Install &amp; Relaunch"}</button>`
      : "";
    const dlOnlyBtn = upd && upd.update_available && upd.asset_url && upd.asset_name && !state.updateDownloading
      ? `<button id="btn-download-only" class="btn btn-ghost btn-sm" type="button" title="Download & open installer without auto-replacing">Open Installer</button>`
      : "";
    const revealBtn = state.updateDownloadedPath
      ? `<button id="btn-reveal-update" class="btn btn-secondary btn-sm" type="button">Reveal Installer</button>`
      : "";
    const progStageTxt = prog?.status === "preparing"
      ? `Preparing bundle &amp; stripping quarantine attributes for ${esc(upd?.asset_name || "")}…`
      : prog?.status === "installing"
        ? `Applying update and restarting Media Shack…`
        : `Downloading ${esc(upd?.asset_name || "")}: ${fmtBytes(prog?.downloaded_bytes || 0)}${prog?.total_bytes ? ` / ${fmtBytes(prog.total_bytes)}` : ""}${pct != null ? ` (${pct.toFixed(0)}%)` : ""}`;
    const progressHtml = state.updateDownloading && prog
      ? `<div style="margin-top:8px;">
           <div class="muted" style="font-size:12px;">${progStageTxt}</div>
           <div class="update-progress-track"><div class="update-progress-fill" style="width:${pct != null ? pct.toFixed(1) : 35}%;"></div></div>
         </div>`
      : "";
    const downloadedNote = state.updateDownloadedPath && !state.updateDownloading
      ? `<div class="muted" style="font-size:12px;margin-top:6px;">Saved to <span class="mono">${esc(state.updateDownloadedPath)}</span></div>`
      : "";
    const notesHtml = upd && upd.update_available && upd.release_notes
      ? `<div class="update-release-notes">${esc(upd.release_notes)}</div>`
      : "";

    return `
      <div class="inline-row" style="justify-content:space-between;gap:10px;flex-wrap:wrap;margin-bottom:8px;">
        <div>
          <h3 style="margin:0;">Software Updates (GitHub Releases)</h3>
          <div class="muted" style="font-size:12px;">Installed: <span class="mono">v${esc(curVer)}</span> · ${esc(upd?.platform_label || "Desktop Release")}</div>
        </div>
        <div class="inline-row" style="gap:8px;flex-wrap:wrap;">
          <label class="toggle-inline" style="font-size:12px;">
            <input id="chk-auto-update" type="checkbox" ${state.autoUpdateCheck ? "checked" : ""} />
            <span>Check on launch</span>
          </label>
          <button id="btn-check-updates" class="btn btn-secondary btn-sm" type="button" ${state.updateChecking ? "disabled" : ""}>${state.updateChecking ? "Checking…" : "Check for Updates"}</button>
          ${downloadBtn}
          ${dlOnlyBtn}
          ${revealBtn}
          <button id="btn-open-releases" class="btn btn-ghost btn-sm" type="button">GitHub Releases</button>
        </div>
      </div>
      <div>${statusLine}</div>
      ${progressHtml}
      ${downloadedNote}
      ${notesHtml}
    `;
  }

  function bindUpdatesCardEvents() {
    const chk = $("chk-auto-update");
    if (chk) {
      chk.onchange = (e) => {
        state.autoUpdateCheck = Boolean(e.target.checked);
        localStorage.setItem("ms_auto_update_check", state.autoUpdateCheck ? "true" : "false");
      };
    }
    const btnCheck = $("btn-check-updates");
    if (btnCheck) {
      btnCheck.onclick = () => runUpdateCheck({ silent: false });
    }
    const btnRel = $("btn-open-releases");
    if (btnRel) {
      btnRel.onclick = () => {
        const url = state.updateCheck?.html_url || "https://github.com/linwe2012/media-shack/releases";
        invoke("open_external_url", { url }).catch((e) => alert(String(e)));
      };
    }
    const startUpdate = async (autoInstall) => {
      if (state.updateDownloading || !state.updateCheck?.asset_url || !state.updateCheck?.asset_name) return;
      state.updateDownloading = true;
      state.updateError = null;
      state.updateStatusMessage = null;
      state.updateProgress = { downloaded_bytes: 0, total_bytes: state.updateCheck.asset_size || null, percent: 0, status: "downloading" };
      refreshSettingsCardUpdatesOnly();
      try {
        const res = await invoke("download_and_install_update", {
          assetUrl: state.updateCheck.asset_url,
          assetName: state.updateCheck.asset_name,
          autoLaunch: true,
          autoInstall,
          forceQuitActive: false,
        });
        state.updateDownloadedPath = res.downloaded_path;
        if (res.message) state.updateStatusMessage = res.message;
      } catch (e) {
        state.updateError = String(e);
      } finally {
        state.updateDownloading = false;
        refreshSettingsCardUpdatesOnly();
      }
    };
    const btnDl = $("btn-download-update");
    if (btnDl) btnDl.onclick = () => startUpdate(true);
    const btnDlOnly = $("btn-download-only");
    if (btnDlOnly) btnDlOnly.onclick = () => startUpdate(false);
    const btnRev = $("btn-reveal-update");
    if (btnRev && state.updateDownloadedPath) {
      btnRev.onclick = () => invoke("reveal_output", { path: state.updateDownloadedPath }).catch((e) => alert(String(e)));
    }
  }

  function refreshSettingsCardUpdatesOnly() {
    const card = $("settings-updates-card");
    if (!card) return;
    card.innerHTML = renderUpdatesCardHtml();
    bindUpdatesCardEvents();
  }

  async function refreshSettingsView() {
    const [tools, faceEnabled, llmCfg, ocrList, aliases] = await Promise.all([
      invoke("metadata_check_tools"),
      invoke("faces_get_enabled"),
      invoke("llm_get_config"),
      invoke("ocr_list_detections", { reviewStatus: "pending" }),
      invoke("ocr_list_creator_aliases"),
    ]);

    $("settings-workspace").innerHTML = `
      <div class="card-panel">
        <div class="inline-row" style="justify-content:space-between;margin-bottom:8px;">
          <div>
            <h3 style="margin:0;">Appearance &amp; Theme</h3>
            <div class="muted" style="font-size:12px;">Choose light or dark mode and workspace accent palette.</div>
          </div>
          <button id="btn-set-done" class="btn btn-sm btn-secondary" type="button">Back to Resource Tabs</button>
        </div>
        <div class="settings-appearance-row">
          <div class="field-block settings-field">
            <span class="field-label">Theme Mode</span>
            <div class="mode-switcher settings-seg" role="group" aria-label="Theme Mode">
              <button type="button" class="tab-mode-btn ${state.theme === "auto" ? "active" : ""}" data-theme-pick="auto">◐ Auto</button>
              <button type="button" class="tab-mode-btn ${state.theme === "dark" ? "active" : ""}" data-theme-pick="dark">☾ Dark</button>
              <button type="button" class="tab-mode-btn ${state.theme === "light" ? "active" : ""}" data-theme-pick="light">☀ Light</button>
            </div>
          </div>
          <div class="field-block settings-field">
            <span class="field-label">Accent Color</span>
            <div class="inline-row" style="gap:8px;flex-wrap:wrap;align-items:center;">
              <div class="mode-switcher settings-seg" role="group" aria-label="Accent Palette">
                <button type="button" class="tab-mode-btn ${currentAccent === "neutral" ? "active" : ""}" data-accent-pick="neutral">
                  <span class="accent-dot" data-accent-pick="neutral" style="pointer-events:none"></span>
                  <span>Neutral</span>
                </button>
                <button type="button" class="tab-mode-btn ${currentAccent === "amber" ? "active" : ""}" data-accent-pick="amber">
                  <span class="accent-dot" data-accent-pick="amber" style="pointer-events:none"></span>
                  <span>Amber</span>
                </button>
                <button type="button" class="tab-mode-btn ${currentAccent === "sage" ? "active" : ""}" data-accent-pick="sage">
                  <span class="accent-dot" data-accent-pick="sage" style="pointer-events:none"></span>
                  <span>Sage</span>
                </button>
              </div>
              <div class="mode-switcher settings-seg" role="group" aria-label="Custom Accent Swatches">
                ${["#3B82F6", "#8B5CF6", "#EC4899", "#14B8A6"].map((hex) => `
                  <button type="button" class="settings-swatch-btn ${currentAccent === "custom" && currentCustomHex === hex ? "active" : ""}" data-swatch-hex="${hex}" title="Use ${hex}">
                    <span class="settings-swatch-fill" style="background:${hex};"></span>
                  </button>
                `).join("")}
                <label class="settings-color-picker-pill ${currentAccent === "custom" ? "active" : ""}" title="Pick any custom accent color">
                  <input id="inp-custom-accent" type="color" value="${esc(currentCustomHex)}" aria-label="Custom accent color" />
                  <span id="lbl-custom-accent-hex" class="mono">${esc(currentAccent === "custom" ? currentCustomHex : (PRESET_ACCENT_HEX[currentAccent] || currentCustomHex))}</span>
                </label>
              </div>
            </div>
          </div>
        </div>
      </div>

      <div id="settings-updates-card" class="card-panel">
        ${renderUpdatesCardHtml()}
      </div>

      <div class="card-panel">
        <h3>Media Tools &amp; Catalog Database</h3>
        <div class="inline-row" style="gap:8px;flex-wrap:wrap;margin-bottom:8px;">
          <button id="btn-set-ffmpeg" class="btn btn-secondary btn-sm" type="button">Configure FFmpeg &amp; ffprobe…</button>
          <button id="btn-db-backup" class="btn btn-secondary btn-sm" type="button">Backup SQLite Catalog…</button>
        </div>
        <div>ExifTool: <span class="mono">${esc(tools.exiftool_path || "Not found (optional)")}</span></div>
        <div>PAR2: <span class="mono">${esc(tools.par2_path || "Not found (optional)")}</span></div>
        <div>ML Worker (PaddleOCR/ONNX): <span class="mono">${esc(tools.ml_worker_path || "Built-in Rust engine")}</span></div>
      </div>
      <div class="card-panel">
        <h3>Privacy &amp; Optional Intelligence</h3>
        <div class="sidebar-panel-stack" style="gap:10px;">
          <label class="check-row-card" style="cursor:pointer;">
            <span>Enable opt-in local anonymous Face Grouping (People tab)</span>
            <input id="chk-set-faces" type="checkbox" class="switch-chk" ${faceEnabled ? "checked" : ""} />
          </label>
          <div class="compact-filter-row" style="gap:10px;">
            <label class="check-row" style="gap:8px;"><input id="chk-llm-en" type="checkbox" class="switch-chk" ${llmCfg.enabled ? "checked" : ""} /><span>Enable Local LLM</span></label>
          <select id="sel-llm-prov"><option value="ollama" ${llmCfg.provider === "ollama" ? "selected" : ""}>Ollama</option><option value="lm_studio" ${llmCfg.provider === "lm_studio" ? "selected" : ""}>LM Studio</option><option value="openai_compat" ${llmCfg.provider === "openai_compat" ? "selected" : ""}>OpenAI-Compat</option></select>
          <input id="inp-llm-url" type="text" value="${esc(llmCfg.endpoint_url)}" style="width:190px" />
          <input id="inp-llm-model" type="text" value="${esc(llmCfg.model_name)}" style="width:120px" />
          <input id="inp-llm-key" type="password" value="${esc(llmCfg.api_key || "")}" placeholder="Optional API key" style="width:120px" />
          <button id="btn-save-llm" class="btn btn-primary btn-sm" type="button">Save LLM Config</button>
        </div>
        </div>
      </div>
      <div class="card-panel">
        <div class="inline-row" style="justify-content:space-between;margin-bottom:8px">
          <h3 style="margin:0">OCR &amp; Watermark Review Queue (${ocrList.length} pending)</h3>
          ${ocrList.length ? `<button id="btn-ocr-approve-all" class="btn btn-secondary btn-sm" type="button">Batch Approve All Creator Tags</button>` : ""}
        </div>
        <div class="inline-row" style="margin-bottom:8px">
          <input id="inp-alias-from" type="text" placeholder="OCR alias e.g. @user_old" />
          <input id="inp-alias-to" type="text" placeholder="Canonical creator e.g. UserName" />
          <button id="btn-add-alias" class="btn btn-secondary btn-sm" type="button">Save Creator Alias</button>
          <span class="muted">${Object.keys(aliases).length} aliases</span>
        </div>
        <table class="table-simple">
          <thead><tr><th>Text (Normalized)</th><th>Class</th><th>Conf / Freq</th><th>Suggested Tag</th><th>Review</th></tr></thead>
          <tbody>${ocrList.map((o) => `<tr>
            <td>${esc(o.normalized_text)}</td>
            <td>${esc(o.classification)}</td>
            <td>${(o.confidence * 100).toFixed(0)}% · ${o.frequency}×</td>
            <td>${esc(o.suggested_creator_tag || "—")}</td>
            <td>
              <button class="btn btn-primary btn-sm" data-ocr-app="${esc(o.id)}">Approve</button>
              <button class="btn btn-ghost btn-sm" data-ocr-rej="${esc(o.id)}">Reject</button>
            </td>
          </tr>`).join("")}</tbody>
        </table>
      </div>`;

    $("settings-workspace").querySelectorAll("[data-theme-pick]").forEach((btn) => {
      btn.onclick = () => {
        state.theme = btn.getAttribute("data-theme-pick") || "auto";
        localStorage.setItem("ms_theme", state.theme);
        applyTheme();
      };
    });
    $("settings-workspace").querySelectorAll("[data-accent-pick]").forEach((btn) => {
      btn.onclick = () => {
        applyAccent(btn.getAttribute("data-accent-pick"));
        $("settings-workspace").querySelectorAll("[data-swatch-hex]").forEach((s) => s.classList.remove("active"));
        $("settings-workspace").querySelector(".settings-color-picker-pill")?.classList.remove("active");
      };
    });
    $("settings-workspace").querySelectorAll("[data-swatch-hex]").forEach((btn) => {
      btn.onclick = () => {
        const hex = btn.getAttribute("data-swatch-hex");
        applyAccent("custom", hex);
        $("settings-workspace").querySelectorAll("[data-swatch-hex]").forEach((s) => s.classList.toggle("active", s === btn));
        $("settings-workspace").querySelector(".settings-color-picker-pill")?.classList.add("active");
      };
    });
    const customAccentInp = $("inp-custom-accent");
    if (customAccentInp) {
      customAccentInp.addEventListener("input", (e) => {
        applyAccent("custom", e.target.value);
        $("settings-workspace").querySelectorAll("[data-swatch-hex]").forEach((s) => s.classList.toggle("active", s.getAttribute("data-swatch-hex") === currentCustomHex));
        $("settings-workspace").querySelector(".settings-color-picker-pill")?.classList.add("active");
      });
    }
    $("btn-set-done")?.addEventListener("click", () => switchView("transcode"));
    bindUpdatesCardEvents();
    $("btn-set-ffmpeg")?.addEventListener("click", () => window.dispatchEvent(new CustomEvent("ms-open-tools-modal")));
    $("btn-db-backup").onclick = async () => {
      const dest = await dialog.save({ defaultPath: "media_shack_backup.db" });
      if (!dest) return;
      await invoke("catalog_backup_database", { destPath: dest });
      alert(`Catalog backed up cleanly to ${dest}`);
    };
    $("chk-set-faces").onchange = async (e) => {
      await invoke("faces_set_enabled", { enabled: e.target.checked });
      window.dispatchEvent(new CustomEvent("ms-faces-enabled-changed", { detail: { enabled: e.target.checked } }));
    };
    $("btn-save-llm").onclick = async () => {
      await invoke("llm_save_config", {
        config: {
          enabled: $("chk-llm-en").checked,
          provider: $("sel-llm-prov").value,
          endpoint_url: $("inp-llm-url").value.trim(),
          model_name: $("inp-llm-model").value.trim(),
          api_key: $("inp-llm-key").value.trim() || null,
        },
      });
      alert("Saved local LLM config.");
    };
    if ($("btn-ocr-approve-all")) {
      $("btn-ocr-approve-all").onclick = async () => {
        const ids = ocrList.filter((o) => o.suggested_creator_tag).map((o) => o.id);
        if (!ids.length) return alert("No pending detections have reliable creator tag suggestions.");
        const n = await invoke("ocr_review_batch", { detectionIds: ids, approve: true, tagPrefix: "creator" });
        alert(`Approved ${n} creator suggestion(s).`);
        refreshSettingsView();
      };
    }
    $("btn-add-alias").onclick = async () => {
      await invoke("ocr_save_creator_alias", { alias: $("inp-alias-from").value, canonical: $("inp-alias-to").value });
      refreshSettingsView();
    };
    $("settings-workspace").querySelectorAll("[data-ocr-app]").forEach((b) => {
      b.onclick = () => invoke("ocr_review_batch", { detectionIds: [b.dataset.ocrApp], approve: true, tagPrefix: "creator" }).then(refreshSettingsView);
    });
    $("settings-workspace").querySelectorAll("[data-ocr-rej]").forEach((b) => {
      b.onclick = () => invoke("ocr_review_batch", { detectionIds: [b.dataset.ocrRej], approve: false, tagPrefix: null }).then(refreshSettingsView);
    });
  }

  // Listen to backend catalog/tasks/duplicates events
  event.listen("catalog_changed", () => { if (state.currentView === "library") refreshLibrary(); });
  event.listen("duplicates_changed", () => { if (state.currentView === "duplicates") refreshDuplicates(); });
  event.listen("tasks_changed", () => { refreshTasksView(); });
  event.listen("update_download_progress", (e) => {
    state.updateProgress = e.payload;
    if (state.currentView === "settings") refreshSettingsCardUpdatesOnly();
  });

  // Boot into Resource Tabs Workspace while hydrating footer catalog stats
  switchView("transcode");
  invoke("faces_get_enabled")
    .then((en) => window.dispatchEvent(new CustomEvent("ms-faces-enabled-changed", { detail: { enabled: Boolean(en) } })))
    .catch(() => {});
  if (state.autoUpdateCheck) {
    setTimeout(() => runUpdateCheck({ silent: true }), 2500);
  }
  invoke("catalog_get_stats").then((stats) => {
    if ($("lbl-catalog-footer")) {
      $("lbl-catalog-footer").textContent =
        `Catalog: ${stats.total_files} files (${stats.online_files} online) · ${fmtBytes(stats.total_bytes)}`;
    }
  }).catch(() => {});
})();
