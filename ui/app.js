(() => {
  const { core, event, dialog, window: tauriWindow } = window.__TAURI__;
  const invoke = core.invoke;

  let snapshot = null;
  let selectedJobId = null;
  let checkedJobIds = new Set();
  let expandedJobIds = new Set();
  let filterNeedsReview = false;
  let livePreviewTimer = null;
  let currentLiveJobId = null;
  let openSampleRefresh = null;
  let stagingState = null;

  const $ = (id) => document.getElementById(id);

  function fmtBytes(bytes) {
    if (bytes === null || bytes === undefined) return "—";
    if (bytes === 0) return "0 B";
    const units = ["B", "KiB", "MiB", "GiB", "TiB"];
    let val = bytes;
    let i = 0;
    while (val >= 1024 && i < units.length - 1) {
      val /= 1024;
      i++;
    }
    return `${val.toFixed(i === 0 ? 0 : val >= 100 ? 1 : 2)} ${units[i]}`;
  }

  function fmtDuration(secs) {
    if (secs === null || secs === undefined || !isFinite(secs) || secs <= 0) return "—";
    const s = Math.floor(secs);
    const h = Math.floor(s / 3600);
    const m = Math.floor((s % 3600) / 60);
    const r = s % 60;
    if (h > 0) return `${h}:${String(m).padStart(2, "0")}:${String(r).padStart(2, "0")}`;
    return `${m}:${String(r).padStart(2, "0")}`;
  }

  function fmtRatio(inputBytes, outputBytes) {
    if (!inputBytes || !outputBytes || outputBytes === 0) return "—";
    const ratio = inputBytes / outputBytes;
    const pct = 100 * (1 - outputBytes / inputBytes);
    if (pct >= 0) {
      return `${ratio.toFixed(2)}:1 · ${Math.round(pct)}% smaller`;
    }
    return `${ratio.toFixed(2)}:1 · ${Math.round(Math.abs(pct))}% larger`;
  }

  function activeQueue() {
    if (!snapshot) return null;
    return snapshot.queues.find((q) => q.id === snapshot.active_queue_id) || snapshot.queues[0] || null;
  }

  function activeQueueJobs() {
    const q = activeQueue();
    if (!q || !snapshot) return [];
    return snapshot.jobs.filter((j) => j.queue_id === q.id);
  }

  async function refreshSnapshot() {
    snapshot = await invoke("get_snapshot");
    renderAll();
  }

  function renderAll() {
    if (!snapshot) return;
    renderTabs();
    renderToolsPill();
    const q = activeQueue();
    const isRepair = q && q.tab_type === "repair";
    $("encode-toolbar").classList.toggle("hidden", !!isRepair);
    $("encode-config-strip").classList.toggle("hidden", !!isRepair);
    $("encode-workspace").classList.toggle("hidden", !!isRepair);
    $("repair-workspace").classList.toggle("hidden", !isRepair);
    if (isRepair) {
      $("banner-area").innerHTML = "";
      renderRepairWorkspace();
    } else {
      renderToolbar();
      renderConfigStrip();
      renderBanners();
      renderQueueList();
      renderSidePanel();
    }
    renderFooter();
  }

  function renderTabs() {
    const strip = $("tabs-strip");
    strip.innerHTML = "";
    for (const q of snapshot.queues) {
      const isActive = q.id === snapshot.active_queue_id;
      const item = document.createElement("div");
      item.className = "tab-item" + (isActive ? " active" : "");

      const btn = document.createElement("button");
      btn.type = "button";
      btn.role = "tab";
      btn.className = "tab-select-btn";
      btn.setAttribute("aria-selected", String(isActive));

      const dot = document.createElement("span");
      const sess = (snapshot.repair_sessions || []).find((s) => s.queue_id === q.id);
      const repairBusy = q.tab_type === "repair" && sess && (sess.status === "diagnosing" || sess.status === "recovering");
      dot.className = "dot " + ((q.is_running && !q.is_paused) || repairBusy ? "pulse" : q.is_paused ? "" : "ok");
      btn.appendChild(dot);

      const label = document.createElement("span");
      label.textContent = q.name;
      btn.appendChild(label);

      btn.addEventListener("click", () => invoke("select_queue_tab", { queueId: q.id }));
      btn.addEventListener("dblclick", () => {
        openPromptModal("Rename Tab", "Tab name:", q.name, async (next) => {
          await invoke("rename_queue_tab", { queueId: q.id, name: next });
        });
      });
      item.appendChild(btn);

      if (isActive) {
        const modeSel = document.createElement("select");
        modeSel.className = "tab-mode-select";
        modeSel.title = "Switch tab feature mode";
        modeSel.setAttribute("aria-label", "Tab mode");
        modeSel.innerHTML = `
          <option value="encode">Encode</option>
          <option value="repair">Repair</option>
        `;
        modeSel.value = q.tab_type || "encode";
        modeSel.addEventListener("click", (e) => e.stopPropagation());
        modeSel.addEventListener("change", async (e) => {
          e.stopPropagation();
          try {
            await invoke("set_queue_tab_type", { queueId: q.id, tabType: e.target.value });
          } catch (err) {
            alert(err);
          }
        });
        item.appendChild(modeSel);
      } else {
        const pill = document.createElement("span");
        pill.className = "tab-type-pill";
        pill.textContent = q.tab_type === "repair" ? "Repair" : "Encode";
        item.appendChild(pill);
      }

      if (snapshot.queues.length > 1) {
        const close = document.createElement("button");
        close.type = "button";
        close.className = "tab-close";
        close.textContent = "×";
        close.title = "Close tab";
        close.addEventListener("click", async (e) => {
          e.stopPropagation();
          try {
            await invoke("delete_queue_tab", { queueId: q.id });
          } catch (err) {
            alert(err);
          }
        });
        item.appendChild(close);
      }
      strip.appendChild(item);
    }
  }

  function renderToolsPill() {
    const ts = snapshot.tools;
    const dot = $("tool-dot");
    const lbl = $("tool-label");
    if (ts.discovered && ts.capabilities) {
      dot.className = "dot ok";
      const codecs = ts.capabilities.available_codecs.map((c) => c.toUpperCase()).join("+") || "Remux only";
      lbl.textContent = `FFmpeg ready (${codecs})`;
    } else {
      dot.className = "dot err";
      lbl.textContent = "Locate FFmpeg tools";
    }
  }

  function renderToolbar() {
    const q = activeQueue();
    if (!q) return;
    $("chk-recursive").checked = q.recursive_import;
    $("lbl-dest-root").textContent = q.destination_root || "Auto (next to source)";

    const vol = findQueueVolumeInfo(q);
    if (vol) {
      const resStr = vol.reserve_bytes > 0 ? ` · reserve ${fmtBytes(vol.reserve_bytes)}` : "";
      $("lbl-dest-space").textContent = `${fmtBytes(vol.free_bytes)} free${resStr}`;
    } else {
      $("lbl-dest-space").textContent = "Reserve: 0 B";
    }

    const polMap = {
      keep_originals: "Keep originals",
      review_after_completion: "Review after completion",
      trash_after_each_file: "Trash verified originals",
    };
    $("lbl-cleanup-policy").textContent = polMap[q.cleanup_policy] || "Keep originals";

    const jobs = activeQueueJobs();
    const readyCount = jobs.filter((j) => j.status === "ready" || j.status === "queued").length;
    const reviewCount = jobs.filter((j) => j.status === "needs_attention").length;
    const interCount = jobs.filter((j) => j.status === "interrupted").length;

    const parts = [`${readyCount} ready`];
    if (reviewCount > 0) parts.push(`${reviewCount} need review`);
    if (interCount > 0) parts.push(`${interCount} interrupted`);
    $("lbl-counts").textContent = parts.join(" · ") + (filterNeedsReview ? " (filtered)" : "");

    const activeInQueue = jobs.some((j) => ["encoding", "verifying", "publishing"].includes(j.status));
    const startBtn = $("btn-start-queue");
    const pauseBtn = $("btn-pause-after");
    const cancelBtn = $("btn-cancel-current");

    if (q.is_running && !q.is_paused) {
      startBtn.classList.add("hidden");
      pauseBtn.classList.remove("hidden");
      pauseBtn.textContent = q.pause_after_current ? "Pausing after current…" : "Pause after current file";
      pauseBtn.disabled = q.pause_after_current;
      cancelBtn.classList.toggle("hidden", !activeInQueue);
    } else {
      startBtn.classList.remove("hidden");
      pauseBtn.classList.add("hidden");
      cancelBtn.classList.add("hidden");
      if (q.is_paused || interCount > 0) {
        startBtn.textContent = `Resume queue (${readyCount + interCount})`;
      } else {
        startBtn.textContent = readyCount > 0 ? `Start ${readyCount} ready files` : "Start queue";
      }
      startBtn.disabled = readyCount === 0 && interCount === 0;
    }
  }

  function findQueueVolumeInfo(q) {
    if (!snapshot.volumes.length) return null;
    if (q.destination_root) {
      return (
        snapshot.volumes.find((v) => q.destination_root.startsWith(v.mount_point)) ||
        snapshot.volumes[0]
      );
    }
    return snapshot.volumes[0];
  }

  function renderConfigStrip() {
    const q = activeQueue();
    if (!q) return;

    // Profiles dropdown
    const selProf = $("sel-profile");
    selProf.innerHTML = `<option value="">(No profile / Custom)</option>`;
    for (const p of snapshot.profiles) {
      const opt = document.createElement("option");
      opt.value = p.id;
      opt.textContent = p.name;
      if (q.profile_id === p.id) opt.selected = true;
      selProf.appendChild(opt);
    }

    const checkedWaitingCount = activeQueueJobs().filter(
      (j) => checkedJobIds.has(j.id) && ["queued", "ready", "needs_attention", "interrupted", "failed", "canceled"].includes(j.status)
    ).length;
    const waitingCount = activeQueueJobs().filter((j) =>
      ["queued", "ready", "needs_attention", "interrupted", "failed", "canceled"].includes(j.status)
    ).length;
    $("btn-apply-waiting").textContent = checkedWaitingCount > 0
      ? `Apply to ${checkedWaitingCount} selected`
      : `Apply to ${waitingCount} waiting files`;
    $("btn-apply-waiting").disabled = waitingCount === 0;

    const r = q.recipe;
    $("sel-family").value = r.family;
    $("sel-variant").value = r.variant;
    $("sel-codec").value = r.codec;
    $("sel-container").value = r.container;

    const isRemux = r.family === "remux";
    $("wrap-encode-controls").classList.toggle("hidden", isRemux);
    $("adv-encode-fields").classList.toggle("hidden", isRemux);
    $("badge-custom").classList.toggle("hidden", !r.is_custom);
    $("btn-reset-preset").classList.toggle("hidden", !r.is_custom);

    // Advanced fields
    $("lbl-crf-range").textContent = r.codec === "hevc" ? "0–51" : "0–63";
    $("inp-crf").min = 0;
    $("inp-crf").max = r.codec === "hevc" ? 51 : 63;
    $("inp-crf").value = r.crf;

    const efforts = r.codec === "hevc" ? ["medium", "slow", "slower", "veryslow"] : ["3", "4", "5", "6", "7"];
    const selEffort = $("sel-effort");
    selEffort.innerHTML = "";
    for (const ef of efforts) {
      const opt = document.createElement("option");
      opt.value = ef;
      opt.textContent = r.codec === "av1" ? `preset ${ef}` : ef;
      if (String(r.effort) === ef) opt.selected = true;
      selEffort.appendChild(opt);
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

    $("sel-priority").value = q.priority;
    $("sel-error-policy").value = q.error_policy;
    $("sel-cleanup-policy").value = q.cleanup_policy;
    $("chk-keep-awake").checked = q.keep_awake;
    $("chk-full-decode").checked = q.full_decode_verify;
    $("chk-min-savings").checked = q.min_savings_enabled;
    $("inp-min-savings-pct").value = q.min_savings_pct;

    const hasJobs = activeQueueJobs().length > 0;
    $("btn-preview-15s").disabled = !selectedJobId && !hasJobs;
  }

  function renderBanners() {
    const area = $("banner-area");
    area.innerHTML = "";
    const q = activeQueue();
    if (!q) return;

    if (!snapshot.tools.discovered) {
      const b = document.createElement("div");
      b.className = "banner banner-warn";
      b.innerHTML = `<span><strong>FFmpeg tools not detected.</strong> ${snapshot.tools.repair_hint || ""}</span>`;
      const btn = document.createElement("button");
      btn.className = "btn btn-sm btn-primary";
      btn.textContent = "Locate FFmpeg tools";
      btn.onclick = openToolsModal;
      b.appendChild(btn);
      area.appendChild(b);
    } else if (
      snapshot.tools.capabilities &&
      q.recipe.family === "archive" &&
      q.recipe.codec === "hevc" &&
      !snapshot.tools.capabilities.has_libx265
    ) {
      const b = document.createElement("div");
      b.className = "banner banner-warn";
      b.innerHTML = `<span><strong>HEVC (libx265) is unavailable in the detected FFmpeg build.</strong> Switch to an available alternative before encoding.</span>`;
      const acts = document.createElement("div");
      acts.style.display = "flex";
      acts.style.gap = "6px";
      if (snapshot.tools.capabilities.has_libsvtav1) {
        const av1Btn = document.createElement("button");
        av1Btn.className = "btn btn-sm btn-primary";
        av1Btn.textContent = "Use AV1 (Balanced)";
        av1Btn.onclick = () => patchActiveQueueSettings((p) => { p.recipe.codec = "av1"; applyCodecDefaults(p.recipe); });
        acts.appendChild(av1Btn);
      }
      const rmxBtn = document.createElement("button");
      rmxBtn.className = "btn btn-sm";
      rmxBtn.textContent = "Use Remux";
      rmxBtn.onclick = () => patchActiveQueueSettings((p) => { p.recipe.family = "remux"; applyCodecDefaults(p.recipe); });
      acts.appendChild(rmxBtn);
      b.appendChild(acts);
      area.appendChild(b);
    }

    if (q.waiting_reason) {
      const b = document.createElement("div");
      b.className = "banner banner-warn";
      b.innerHTML = `<span><strong>${q.name}:</strong> ${escapeHtml(q.waiting_reason)}</span>`;
      const acts = document.createElement("div");
      acts.style.display = "flex";
      acts.style.gap = "6px";
      const recheck = document.createElement("button");
      recheck.className = "btn btn-sm";
      recheck.textContent = "Recheck volumes";
      recheck.onclick = () => invoke("recheck_volumes");
      acts.appendChild(recheck);
      b.appendChild(acts);
      area.appendChild(b);
    }

    const ms = snapshot.morning_summary;
    if (ms && ms.queue_id === q.id && (ms.completed_count > 0 || ms.failed_count > 0 || ms.below_savings_count > 0)) {
      const b = document.createElement("div");
      b.className = "banner banner-ok";
      const ratioStr = ms.total_output_bytes > 0
        ? fmtRatio(ms.total_input_bytes, ms.total_output_bytes)
        : "—";
      const cleanNote =
        ms.trashed_count > 0 && ms.kept_count === 0
          ? `${ms.trashed_count} originals trashed`
          : ms.trashed_count > 0
          ? `${ms.trashed_count} trashed, ${ms.kept_count} kept`
          : "originals kept";
      const vol = findQueueVolumeInfo(q);
      const freeTxt = vol ? ` · Dest free: ${fmtBytes(vol.free_bytes)}` : "";
      b.innerHTML = `<span><strong>Summary:</strong> ${ms.completed_count} completed (${fmtBytes(ms.total_input_bytes)} → ${fmtBytes(ms.total_output_bytes)} · ${ratioStr} · ${cleanNote})${ms.failed_count ? ` · ${ms.failed_count} failed` : ""}${ms.below_savings_count ? ` · ${ms.below_savings_count} below savings target` : ""}${freeTxt}</span>`;

      const acts = document.createElement("div");
      acts.style.display = "flex";
      acts.style.gap = "6px";
      const firstDone = activeQueueJobs().find((j) => j.status === "completed");
      if (firstDone) {
        const showBtn = document.createElement("button");
        showBtn.className = "btn btn-sm";
        showBtn.textContent = "Show output";
        showBtn.onclick = () => invoke("reveal_output", { path: firstDone.destination_path });
        acts.appendChild(showBtn);
      }
      if (ms.kept_count > 0 || ms.cleanup_uncertain_count > 0 || ms.cleanup_failed_count > 0) {
        const revBtn = document.createElement("button");
        revBtn.className = "btn btn-sm btn-primary";
        revBtn.textContent = "Review originals";
        revBtn.onclick = () => openReviewOriginalsModal(q.id);
        acts.appendChild(revBtn);
      }
      b.appendChild(acts);
      area.appendChild(b);
    }
  }

  function renderQueueList() {
    const allQJobs = activeQueueJobs();
    const emptyEl = $("empty-drop-state");
    const listWrap = $("queue-list-wrap");
    const q = activeQueue();

    if (allQJobs.length === 0) {
      emptyEl.classList.remove("hidden");
      listWrap.classList.add("hidden");
      if (q) {
        const tag = q.recipe.family === "remux" ? "Remux" : `Archive → ${q.recipe.variant === "best" ? "Best" : "Balanced"} · ${q.recipe.codec.toUpperCase()} / ${q.recipe.container.toUpperCase()}`;
        $("empty-defaults-summary").textContent = `${tag} · Keep originals`;
      }
      return;
    }

    emptyEl.classList.add("hidden");
    listWrap.classList.remove("hidden");

    const jobs = filterNeedsReview
      ? allQJobs.filter((j) => j.status === "needs_attention" || j.status === "failed")
      : allQJobs;

    $("lbl-queue-filter-bar").textContent = filterNeedsReview
      ? `Showing ${jobs.length} items needing attention or failed (of ${allQJobs.length})`
      : `${allQJobs.length} files in ${q ? q.name : "queue"}`;

    const hasRetryable = allQJobs.some((j) => ["failed", "canceled", "interrupted", "needs_attention"].includes(j.status));
    const hasFinished = allQJobs.some((j) => ["completed", "below_savings_target", "canceled"].includes(j.status));
    $("btn-retry-failed").classList.toggle("hidden", !hasRetryable);
    $("btn-clear-completed").classList.toggle("hidden", !hasFinished);

    const rowsEl = $("queue-rows");
    rowsEl.innerHTML = "";

    for (let idx = 0; idx < jobs.length; idx++) {
      const j = jobs[idx];
      const isSelected = j.id === selectedJobId;
      const isExpanded = expandedJobIds.has(j.id);
      const isActive = j.id === snapshot.active_progress?.job_id;
      const prog = isActive ? snapshot.active_progress : null;

      const row = document.createElement("div");
      row.className = "job-row" + (isSelected ? " selected" : "");
      row.setAttribute("role", "listitem");
      row.addEventListener("click", () => {
        selectedJobId = j.id;
        renderQueueList();
        renderSidePanel();
      });

      const main = document.createElement("div");
      main.className = "job-row-main";

      const selCb = document.createElement("input");
      selCb.type = "checkbox";
      selCb.checked = checkedJobIds.has(j.id);
      selCb.title = "Select row for batch apply/retry";
      selCb.addEventListener("click", (e) => e.stopPropagation());
      selCb.addEventListener("change", () => {
        if (selCb.checked) checkedJobIds.add(j.id);
        else checkedJobIds.delete(j.id);
        renderConfigStrip();
      });
      main.appendChild(selCb);

      const disc = document.createElement("button");
      disc.type = "button";
      disc.className = "disclosure-btn";
      disc.textContent = isExpanded ? "▼" : "▶";
      disc.setAttribute("aria-expanded", String(isExpanded));
      disc.setAttribute("aria-label", `Media details for ${j.source_rel_path}`);
      disc.addEventListener("click", (e) => {
        e.stopPropagation();
        if (expandedJobIds.has(j.id)) expandedJobIds.delete(j.id);
        else expandedJobIds.add(j.id);
        renderQueueList();
      });
      main.appendChild(disc);

      const titleCol = document.createElement("div");
      titleCol.className = "job-title-col";
      const subBits = [
        j.recipe.family === "remux" ? `Remux · ${j.recipe.container.toUpperCase()}` : `${j.recipe.codec.toUpperCase()} CRF ${j.recipe.crf} · ${j.recipe.container.toUpperCase()}`,
        fmtBytes(j.input_bytes),
      ];
      if (j.status === "completed" && j.output_bytes) {
        subBits.push(`→ ${fmtBytes(j.output_bytes)} (${fmtRatio(j.input_bytes, j.output_bytes)})`);
      } else if (j.status === "below_savings_target" && j.output_bytes) {
        subBits.push(`Attempt: ${fmtBytes(j.output_bytes)} · original kept`);
      }
      titleCol.innerHTML = `<div class="job-name" title="${escapeHtml(j.source_path)}">${escapeHtml(j.source_rel_path)}</div>
        <div class="job-sub">${escapeHtml(subBits.join(" · "))}</div>`;
      main.appendChild(titleCol);

      // Status badge
      const statusBadge = document.createElement("span");
      statusBadge.className = "badge " + statusBadgeClass(j.status);
      statusBadge.textContent = statusLabel(j.status);
      main.appendChild(statusBadge);

      // Cleanup badge if completed
      if (j.status === "completed") {
        const cleanBadge = document.createElement("span");
        cleanBadge.className = "badge";
        cleanBadge.textContent = cleanupOutcomeLabel(j.cleanup_outcome);
        main.appendChild(cleanBadge);
      } else {
        const spacer = document.createElement("span");
        main.appendChild(spacer);
      }

      // Row action buttons
      const rowActs = document.createElement("div");
      rowActs.style.display = "flex";
      rowActs.style.gap = "4px";
      if (j.status === "encoding") {
        const lpBtn = document.createElement("button");
        lpBtn.type = "button";
        lpBtn.className = "btn btn-sm btn-secondary";
        lpBtn.textContent = "Live preview";
        lpBtn.onclick = (e) => {
          e.stopPropagation();
          openLivePreviewModal(j.id);
        };
        rowActs.appendChild(lpBtn);
      }
      if (["failed", "canceled", "interrupted", "needs_attention"].includes(j.status)) {
        const rBtn = document.createElement("button");
        rBtn.type = "button";
        rBtn.className = "btn btn-sm";
        rBtn.textContent = "Retry";
        rBtn.onclick = async (e) => {
          e.stopPropagation();
          try {
            await invoke("retry_jobs", { jobIds: [j.id] });
          } catch (err) {
            alert(err);
          }
        };
        rowActs.appendChild(rBtn);
      }
      if (j.status === "completed") {
        const shBtn = document.createElement("button");
        shBtn.type = "button";
        shBtn.className = "btn btn-sm";
        shBtn.textContent = "Show";
        shBtn.onclick = (e) => {
          e.stopPropagation();
          invoke("reveal_output", { path: j.destination_path });
        };
        rowActs.appendChild(shBtn);
      }
      if (["queued", "ready", "needs_attention", "interrupted", "failed", "canceled"].includes(j.status)) {
        const upBtn = document.createElement("button");
        upBtn.type = "button";
        upBtn.className = "btn btn-ghost btn-sm";
        upBtn.textContent = "↑";
        upBtn.title = "Move up";
        upBtn.disabled = idx === 0;
        upBtn.onclick = (e) => {
          e.stopPropagation();
          moveJob(allQJobs, j.id, -1);
        };
        rowActs.appendChild(upBtn);

        const dnBtn = document.createElement("button");
        dnBtn.type = "button";
        dnBtn.className = "btn btn-ghost btn-sm";
        dnBtn.textContent = "↓";
        dnBtn.title = "Move down";
        dnBtn.disabled = idx === jobs.length - 1;
        dnBtn.onclick = (e) => {
          e.stopPropagation();
          moveJob(allQJobs, j.id, 1);
        };
        rowActs.appendChild(dnBtn);

        const rmBtn = document.createElement("button");
        rmBtn.type = "button";
        rmBtn.className = "btn btn-ghost btn-sm";
        rmBtn.textContent = "×";
        rmBtn.title = "Remove from queue";
        rmBtn.onclick = (e) => {
          e.stopPropagation();
          invoke("remove_pending_jobs", { jobIds: [j.id] });
        };
        rowActs.appendChild(rmBtn);
      }
      main.appendChild(rowActs);
      row.appendChild(main);

      // Live Progress Bar
      if (prog || j.status === "verifying" || j.status === "publishing") {
        const pw = document.createElement("div");
        pw.className = "progress-wrap";
        const dur = prog?.duration_secs;
        const pct = dur && dur > 0 ? Math.min(100, (prog.out_time_secs / dur) * 100) : null;
        const indet = pct === null && j.status === "encoding";
        pw.innerHTML = `<div class="progress-track"><div class="progress-fill ${indet ? "indeterminate" : ""}" style="width:${pct !== null ? pct.toFixed(1) : j.status === "encoding" ? 35 : 100}%"></div></div>`;
        const meta = document.createElement("div");
        meta.className = "progress-meta";
        if (j.status === "verifying") {
          meta.textContent = "Verifying streams, timing, and output integrity…";
        } else if (j.status === "publishing") {
          meta.textContent = "Publishing verified output atomically…";
        } else if (prog) {
          const items = [
            `${fmtDuration(prog.out_time_secs)} / ${fmtDuration(prog.duration_secs)}`,
            `Written: ${fmtBytes(prog.bytes_written)}`,
            `Speed: ${prog.speed ? prog.speed.toFixed(2) + "x" : "—"}`,
            `FPS: ${prog.fps ? prog.fps.toFixed(1) : "—"}`,
            `ETA: ${prog.eta_secs ? fmtDuration(prog.eta_secs) : "—"}`,
          ];
          if (prog.est_final_ratio) {
            items.push(`Est. final ratio: ${prog.est_final_ratio.toFixed(2)}:1`);
          }
          meta.textContent = items.join("  ·  ");
        }
        pw.appendChild(meta);
        row.appendChild(pw);
      }

      // Expanded In-Place Input Media Details
      if (isExpanded) {
        const det = document.createElement("div");
        det.className = "job-expand-details";
        det.innerHTML = renderExpandedMediaHtml(j);
        row.appendChild(det);
      }

      rowsEl.appendChild(row);
    }
  }

  function renderExpandedMediaHtml(j) {
    if (!j.probe) {
      return `<p class="muted">No cached ffprobe data available.</p>`;
    }
    const f = j.probe.format;
    const summary = `Container: <strong>${escapeHtml(f.format_long_name || f.format_name || "—")}</strong> · Exact size: <strong>${j.input_bytes.toLocaleString()} bytes</strong> (${fmtBytes(j.input_bytes)}) · Duration: <strong>${fmtDuration(f.duration_secs)}</strong> · Bitrate: <strong>${f.bit_rate ? Math.round(f.bit_rate / 1000) + " kbps" : "—"}</strong>`;

    let rows = "";
    for (const s of j.probe.streams) {
      const dispFlags = [];
      if (s.disposition.default) dispFlags.push("default");
      if (s.disposition.forced) dispFlags.push("forced");
      if (s.disposition.attached_pic) dispFlags.push("cover");
      const langDisp = `${s.language || "—"} ${dispFlags.length ? "(" + dispFlags.join(",") + ")" : ""}`;

      let specs = "—";
      if (s.codec_type === "video") {
        const fr = s.is_vfr
          ? `${s.avg_frame_rate || "—"} (VFR avg)`
          : (s.r_frame_rate || s.avg_frame_rate || "—");
        const color = [s.color_primaries, s.color_transfer, s.color_space].filter(Boolean).join("/") || "—";
        specs = `${s.width || "?"}×${s.height || "?"} · fps: ${fr} · ${s.pix_fmt || "—"} (${s.bit_depth ? s.bit_depth + "-bit" : "—"}) · color: ${color}`;
      } else if (s.codec_type === "audio") {
        specs = `${s.channel_layout || (s.channels ? s.channels + " ch" : "—")} · ${s.sample_rate ? s.sample_rate + " Hz" : "—"} · ${s.bit_rate ? Math.round(s.bit_rate / 1000) + " kbps" : "—"}`;
      } else {
        specs = s.title || "—";
      }

      rows += `<tr>
        <td>#${s.index}</td>
        <td>${escapeHtml(s.disposition.attached_pic ? "cover" : s.codec_type)}</td>
        <td>${escapeHtml(s.codec_name)}</td>
        <td>${escapeHtml(langDisp)}</td>
        <td>${escapeHtml(specs)}</td>
      </tr>`;
    }

    return `<div>${summary}</div>
      <table class="media-tracks-table">
        <thead><tr><th>#</th><th>Type</th><th>Codec</th><th>Language / Flags</th><th>Properties</th></tr></thead>
        <tbody>${rows}</tbody>
      </table>`;
  }

  function renderSidePanel() {
    const panel = $("side-panel-content");
    const j = activeQueueJobs().find((x) => x.id === selectedJobId);
    if (!j) {
      panel.innerHTML = `<p class="muted">Select a job row to inspect its tracks, output destination, pending settings, or recovery actions.</p>`;
      return;
    }

    panel.innerHTML = "";

    // Summary & Output Path
    const s1 = document.createElement("div");
    s1.className = "side-section";
    s1.innerHTML = `<h3>Selected Job</h3>
      <div class="mono" style="word-break:break-all;margin-bottom:6px">${escapeHtml(j.source_rel_path)}</div>
      <div class="muted" style="font-size:11px;word-break:break-all">Output: ${escapeHtml(j.destination_path)}</div>`;
    if (["queued", "ready", "needs_attention", "interrupted", "failed", "canceled"].includes(j.status)) {
      const chBtn = document.createElement("button");
      chBtn.type = "button";
      chBtn.className = "btn btn-sm btn-secondary";
      chBtn.style.marginTop = "6px";
      chBtn.textContent = "Choose destination…";
      chBtn.onclick = async () => {
        const picked = await dialog.save({ defaultPath: j.destination_path });
        if (picked) {
          try {
            await invoke("update_pending_job", { jobId: j.id, patch: { destination_path: picked } });
          } catch (err) {
            alert(err);
          }
        }
      };
      s1.appendChild(chBtn);
    }
    panel.appendChild(s1);

    // Issues & One-Click Recovery Actions
    if (j.error_message || (j.plan && j.plan.issues.length > 0)) {
      const sErr = document.createElement("div");
      sErr.className = "side-section";
      sErr.innerHTML = `<h3>Attention / Error Details</h3>`;
      if (j.error_message) {
        sErr.innerHTML += `<div class="badge badge-err" style="margin-bottom:6px">${escapeHtml(j.error_stage || "error")}</div>
          <p style="margin:4px 0;font-size:12px">${escapeHtml(j.error_message)}</p>`;
      }
      if (j.plan && j.plan.issues.length > 0) {
        for (const iss of j.plan.issues) {
          const card = document.createElement("div");
          card.style.marginTop = "8px";
          card.style.padding = "8px";
          card.style.border = "1px solid var(--border)";
          card.style.borderRadius = "6px";
          card.innerHTML = `<strong>${escapeHtml(iss.summary)}</strong><p style="margin:4px 0;font-size:12px">${escapeHtml(iss.details)}</p>`;
          const acts = document.createElement("div");
          acts.style.display = "flex";
          acts.style.flexWrap = "wrap";
          acts.style.gap = "6px";
          if (iss.can_use_mkv) {
            const mkvBtn = document.createElement("button");
            mkvBtn.type = "button";
            mkvBtn.className = "btn btn-sm btn-primary";
            mkvBtn.textContent = "Use MKV for this file";
            mkvBtn.onclick = () => {
              const nextRecipe = { ...j.recipe, container: "mkv" };
              invoke("update_pending_job", { jobId: j.id, patch: { recipe: nextRecipe } });
            };
            acts.appendChild(mkvBtn);
          }
          if (iss.can_remux) {
            const rmxBtn = document.createElement("button");
            rmxBtn.type = "button";
            rmxBtn.className = "btn btn-sm btn-secondary";
            rmxBtn.textContent = "Remux (stream copy)";
            rmxBtn.onclick = () => {
              const nextRecipe = { ...j.recipe, family: "remux", crf: 0, effort: "none" };
              invoke("update_pending_job", { jobId: j.id, patch: { recipe: nextRecipe } });
            };
            acts.appendChild(rmxBtn);
          }
          if (iss.can_opt_in_fallback && iss.fallback_description) {
            const fbBtn = document.createElement("button");
            fbBtn.type = "button";
            fbBtn.className = "btn btn-sm";
            fbBtn.textContent = iss.fallback_description;
            fbBtn.onclick = () => {
              const ov = { ...j.compat_overrides };
              if (iss.kind === "mp4_incompatible_audio") ov.allow_mp4_audio_aac_fallback = true;
              if (iss.kind === "mp4_incompatible_subtitle") ov.allow_mp4_sub_mov_text_fallback = true;
              invoke("update_pending_job", { jobId: j.id, patch: { compat_overrides: ov } });
            };
            acts.appendChild(fbBtn);
          }
          if (iss.kind === "multiple_moving_video_streams" && j.probe) {
            for (const st of j.probe.streams.filter((s) => s.codec_type === "video" && !s.disposition.attached_pic)) {
              const vBtn = document.createElement("button");
              vBtn.type = "button";
              vBtn.className = "btn btn-sm";
              vBtn.textContent = `Use video #${st.index} (${st.codec_name} ${st.width}×${st.height})`;
              vBtn.onclick = () => {
                const ov = { ...j.compat_overrides, selected_video_stream: st.index };
                invoke("update_pending_job", { jobId: j.id, patch: { compat_overrides: ov } });
              };
              acts.appendChild(vBtn);
            }
          }
          card.appendChild(acts);
          sErr.appendChild(card);
        }
      }
      panel.appendChild(sErr);
    }

    // Track Plan
    if (j.plan) {
      const sPlan = document.createElement("div");
      sPlan.className = "side-section";
      sPlan.innerHTML = `<h3>Track Plan (${j.plan.tracks.length} tracks)</h3>`;
      for (const t of j.plan.tracks) {
        const div = document.createElement("div");
        div.style.fontSize = "12px";
        div.style.marginBottom = "4px";
        div.textContent = `#${t.stream_index} ${t.codec_type} (${t.codec_name}) → ${t.action}${t.reason ? " — " + t.reason : ""}`;
        sPlan.appendChild(div);
      }
      panel.appendChild(sPlan);
    }

    // Details (Printable command & diagnostics)
    const sDet = document.createElement("div");
    sDet.className = "side-section";
    const detBtn = document.createElement("button");
    detBtn.type = "button";
    detBtn.className = "btn btn-sm btn-ghost";
    detBtn.textContent = "Details (FFmpeg command & diagnostics) ▾";
    const detBody = document.createElement("div");
    detBody.className = "hidden";
    detBody.style.marginTop = "6px";
    detBody.innerHTML = `<div class="mono" style="background:var(--bg);padding:6px;border-radius:4px;word-break:break-all;margin-bottom:6px">${escapeHtml(j.plan?.printable_command || "—")}</div>
      ${j.diagnostics ? `<pre class="mono" style="background:var(--bg);padding:6px;border-radius:4px;max-height:180px;overflow:auto;white-space:pre-wrap">${escapeHtml(j.diagnostics)}</pre>` : ""}`;
    detBtn.onclick = () => detBody.classList.toggle("hidden");
    sDet.appendChild(detBtn);
    sDet.appendChild(detBody);
    panel.appendChild(sDet);
  }

  function renderFooter() {
    $("lbl-power-status").textContent = snapshot.power_assertion_active
      ? "Awake assertion active (preventing idle system sleep; display sleep allowed)"
      : "Idle";
    $("lbl-scheduler-status").textContent = snapshot.scheduler_busy_reason || "";
  }

  function statusLabel(st) {
    return {
      queued: "Queued",
      probing: "Probing",
      ready: "Ready",
      needs_attention: "Needs attention",
      encoding: "Encoding",
      verifying: "Verifying",
      publishing: "Publishing",
      completed: "Completed",
      below_savings_target: "Below savings target · original kept",
      failed: "Failed",
      canceled: "Canceled",
      interrupted: "Interrupted",
    }[st] || st;
  }

  function statusBadgeClass(st) {
    if (st === "completed") return "badge-ok";
    if (st === "encoding" || st === "verifying" || st === "publishing") return "badge-active";
    if (st === "failed") return "badge-err";
    if (st === "needs_attention" || st === "interrupted" || st === "below_savings_target") return "badge-warn";
    return "";
  }

  function cleanupOutcomeLabel(co) {
    return {
      none: "Original kept",
      kept: "Original kept",
      pending: "Pending review",
      trashed: "Moved to Trash",
      skipped: "Cleanup skipped",
      declined: "Kept by user",
      uncertain: "Review cleanup",
      failed: "Cleanup failed",
    }[co] || co;
  }

  async function moveJob(jobs, jobId, delta) {
    const ids = jobs.map((j) => j.id);
    const i = ids.indexOf(jobId);
    const j = i + delta;
    if (i < 0 || j < 0 || j >= ids.length) return;
    [ids[i], ids[j]] = [ids[j], ids[i]];
    await invoke("reorder_jobs", { queueId: activeQueue().id, orderedJobIds: ids });
  }

  async function patchActiveQueueSettings(mutator) {
    const q = activeQueue();
    if (!q) return;
    const patch = {
      recipe: { ...q.recipe },
      priority: q.priority,
      recursive_import: q.recursive_import,
      destination_root: q.destination_root,
      keep_awake: q.keep_awake,
      error_policy: q.error_policy,
      min_savings_enabled: q.min_savings_enabled,
      min_savings_pct: q.min_savings_pct,
      cleanup_policy: q.cleanup_policy,
      full_decode_verify: q.full_decode_verify,
    };
    mutator(patch);
    patch.recipe.is_custom = checkRecipeCustom(patch.recipe);
    await invoke("update_queue_settings", { queueId: q.id, patch });
  }

  function checkRecipeCustom(r) {
    if (r.family === "remux") return false;
    const defaults = {
      "best:hevc": [17, "slower"],
      "balanced:hevc": [20, "slow"],
      "best:av1": [22, "3"],
      "balanced:av1": [27, "5"],
    }[`${r.variant}:${r.codec}`];
    if (!defaults) return false;
    return Number(r.crf) !== defaults[0] || String(r.effort) !== defaults[1];
  }

  function applyCodecDefaults(r) {
    if (r.family === "remux") {
      r.crf = 0;
      r.effort = "none";
      r.is_custom = false;
      return;
    }
    const defaults = {
      "best:hevc": [17, "slower"],
      "balanced:hevc": [20, "slow"],
      "best:av1": [22, "3"],
      "balanced:av1": [27, "5"],
    }[`${r.variant}:${r.codec}`];
    if (defaults) {
      r.crf = defaults[0];
      r.effort = defaults[1];
    }
    r.is_custom = false;
  }

  // Import & Staging Flow
  async function startImportFlow(paths) {
    const q = activeQueue();
    if (!q || !paths || paths.length === 0) return;
    openModal("Scanning media…", `<p>Probing dropped/selected items for <strong>${escapeHtml(q.name)}</strong>…</p>`, [
      {
        label: "Cancel scan",
        cls: "btn",
        onClick: async () => {
          await invoke("cancel_import_scan");
          closeModal();
        },
      },
    ]);
    try {
      const scan = await invoke("start_import_scan", {
        queueId: q.id,
        paths,
        recursive: q.recursive_import,
        destinationRootOverride: q.destination_root,
      });
      if (scan.canceled) {
        closeModal();
        return;
      }
      stagingState = scan;
      renderStagingModal();
    } catch (err) {
      openModal("Import Error", `<p>${escapeHtml(String(err))}</p>`, [
        { label: "Close", cls: "btn btn-primary", onClick: closeModal },
      ]);
    }
  }

  function renderStagingModal() {
    const s = stagingState;
    if (!s) return;
    let sortBy = "name";
    const draw = () => {
      const sorted = [...s.items].sort((a, b) => {
        if (sortBy === "size") return b.input_bytes - a.input_bytes;
        if (sortBy === "bitrate") return (b.bit_rate || 0) - (a.bit_rate || 0);
        return a.source_rel_path.localeCompare(b.source_rel_path);
      });
      const includedCount = s.items.filter((i) => i.include).length;
      const body = document.createElement("div");
      body.innerHTML = `
        <div style="display:flex;flex-wrap:wrap;justify-content:space-between;gap:8px;margin-bottom:10px">
          <div>
            <strong>Queue:</strong> ${escapeHtml(s.queue_name)}
            ${s.profile_name ? ` · <strong>Profile:</strong> ${escapeHtml(s.profile_name)}` : ""}
            <div class="muted" style="font-size:12px;margin-top:2px">
              Destination: <span class="mono">${escapeHtml(s.destination_root)}</span>
              <button id="stg-dest-btn" class="btn btn-ghost btn-sm" type="button">Change…</button>
            </div>
            <div class="inline-row" style="margin-top:4px">
              <label>Preset:</label>
              <select id="stg-family">
                <option value="archive" ${s.recipe.family === "archive" ? "selected" : ""}>Archive</option>
                <option value="remux" ${s.recipe.family === "remux" ? "selected" : ""}>Remux</option>
              </select>
              <select id="stg-codec" ${s.recipe.family === "remux" ? "disabled" : ""}>
                <option value="hevc" ${s.recipe.codec === "hevc" ? "selected" : ""}>HEVC</option>
                <option value="av1" ${s.recipe.codec === "av1" ? "selected" : ""}>AV1</option>
              </select>
              <select id="stg-container">
                <option value="mkv" ${s.recipe.container === "mkv" ? "selected" : ""}>MKV</option>
                <option value="mp4" ${s.recipe.container === "mp4" ? "selected" : ""}>MP4</option>
              </select>
            </div>
          </div>
          <div class="inline-row">
            <label>Sort:</label>
            <select id="stg-sort">
              <option value="name" ${sortBy === "name" ? "selected" : ""}>Name</option>
              <option value="size" ${sortBy === "size" ? "selected" : ""}>Input size (largest)</option>
              <option value="bitrate" ${sortBy === "bitrate" ? "selected" : ""}>Bitrate (highest)</option>
            </select>
          </div>
        </div>
        ${s.unreadable_count > 0 ? `<div class="banner banner-warn" style="margin-bottom:8px">${s.unreadable_count} unreadable or blocked items were excluded by default.</div>` : ""}
        <table class="table-simple">
          <thead><tr><th>Add</th><th>File</th><th>Codec / Bitrate</th><th>Size</th><th>Status</th></tr></thead>
          <tbody id="stg-tbody"></tbody>
        </table>`;
      const tbody = body.querySelector("#stg-tbody");
      for (const it of sorted) {
        const tr = document.createElement("tr");
        const stNotes = [];
        if (it.duplicate_in_queue) stNotes.push("Already in queue");
        if (it.destination_collision) stNotes.push("Numbered to avoid collision");
        if (it.unreadable_error) stNotes.push(it.unreadable_error);
        else if (it.plan && !it.plan.is_ready) stNotes.push("Needs review");

        tr.innerHTML = `<td><input type="checkbox" ${it.include ? "checked" : ""} ${it.unreadable_error ? "disabled" : ""}/></td>
          <td><div style="max-width:260px;overflow:hidden;text-overflow:ellipsis">${escapeHtml(it.source_rel_path)}</div></td>
          <td>${escapeHtml(it.current_codec || "—")} · ${it.bit_rate ? Math.round(it.bit_rate / 1000) + " kbps" : "—"}</td>
          <td>${fmtBytes(it.input_bytes)}</td>
          <td>${escapeHtml(stNotes.join("; ") || "Ready")}</td>`;
        const cb = tr.querySelector("input");
        cb.addEventListener("change", () => {
          it.include = cb.checked;
          draw();
        });
        tbody.appendChild(tr);
      }
      body.querySelector("#stg-dest-btn").addEventListener("click", async () => {
        const dir = await dialog.open({ directory: true, defaultPath: s.destination_root });
        if (dir) {
          s.destination_root = dir;
          draw();
        }
      });
      body.querySelector("#stg-family").addEventListener("change", (e) => {
        s.recipe.family = e.target.value;
        applyCodecDefaults(s.recipe);
        draw();
      });
      body.querySelector("#stg-codec").addEventListener("change", (e) => {
        s.recipe.codec = e.target.value;
        applyCodecDefaults(s.recipe);
        draw();
      });
      body.querySelector("#stg-container").addEventListener("change", (e) => {
        s.recipe.container = e.target.value;
        draw();
      });
      body.querySelector("#stg-sort").addEventListener("change", (e) => {
        sortBy = e.target.value;
        draw();
      });

      openModalEl(`Stage Import (${includedCount} of ${s.items.length} selected)`, body, [
        { label: "Cancel", cls: "btn", onClick: closeModal },
        {
          label: `Add ${includedCount} files to ${s.queue_name}`,
          cls: "btn btn-primary",
          disabled: includedCount === 0,
          onClick: async () => {
            const chosen = s.items.filter((i) => i.include).map((i) => ({
              source_path: i.source_path,
              source_rel_path: i.source_rel_path,
              source_root: i.source_root,
              destination_path: i.destination_path,
              input_bytes: i.input_bytes,
              probe: i.probe,
              plan: i.plan,
              unreadable_error: i.unreadable_error,
            }));
            await invoke("commit_staged_import", {
              queueId: s.queue_id,
              destinationRoot: s.destination_root,
              recipe: s.recipe,
              items: chosen,
            });
            closeModal();
          },
        },
      ]);
    };
    draw();
  }

  // Modal helpers
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
    xBtn.className = "btn btn-ghost btn-sm";
    xBtn.textContent = "×";
    xBtn.onclick = closeModal;
    head.appendChild(xBtn);

    const body = document.createElement("div");
    body.className = "modal-body";
    body.appendChild(bodyEl);

    const foot = document.createElement("div");
    foot.className = "modal-foot";
    for (const b of buttons) {
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = b.cls || "btn";
      btn.textContent = b.label;
      btn.disabled = !!b.disabled;
      if (b.autofocus) btn.autofocus = true;
      btn.onclick = b.onClick;
      foot.appendChild(btn);
    }

    box.appendChild(head);
    box.appendChild(body);
    box.appendChild(foot);
    root.classList.remove("hidden");
  }

  function closeModal() {
    $("modal-root").classList.add("hidden");
    openSampleRefresh = null;
    if (livePreviewTimer) {
      clearInterval(livePreviewTimer);
      livePreviewTimer = null;
    }
    if (currentLiveJobId) {
      invoke("stop_live_preview").catch(() => {});
      currentLiveJobId = null;
    }
  }

  function openPromptModal(title, label, defaultValue, onConfirm) {
    const wrap = document.createElement("div");
    wrap.innerHTML = `<div class="field"><label for="prm-input">${escapeHtml(label)}</label><input id="prm-input" type="text" value="${escapeHtml(defaultValue || "")}" style="width:100%" /></div>`;
    const inputEl = wrap.querySelector("#prm-input");
    const submit = async () => {
      const val = inputEl.value.trim();
      if (!val) return;
      closeModal();
      await onConfirm(val);
    };
    inputEl.addEventListener("keydown", (e) => {
      if (e.key === "Enter") {
        e.preventDefault();
        submit();
      }
    });
    openModalEl(title, wrap, [
      { label: "Cancel", cls: "btn", onClick: closeModal },
      { label: "Save", cls: "btn btn-primary", onClick: submit },
    ]);
    setTimeout(() => { inputEl.focus(); inputEl.select(); }, 20);
  }

  // Tools Modal
  function openToolsModal() {
    const ts = snapshot.tools;
    const caps = ts.capabilities;
    let html = "";
    if (caps) {
      html += `<p><strong>ffmpeg:</strong> <span class="mono">${escapeHtml(caps.ffmpeg_path)}</span><br><span class="muted">${escapeHtml(caps.ffmpeg_version)}</span></p>
        <p><strong>ffprobe:</strong> <span class="mono">${escapeHtml(caps.ffprobe_path)}</span><br><span class="muted">${escapeHtml(caps.ffprobe_version)}</span></p>
        <p><strong>Available encoders:</strong> ${caps.available_codecs.map((c) => c.toUpperCase()).join(", ") || "None (Remux only)"}</p>
        <table class="table-simple"><thead><tr><th>Smoke test</th><th>Result</th><th>Details</th></tr></thead><tbody>`;
      for (const st of caps.smoke_tests) {
        html += `<tr><td>${escapeHtml(st.name)}</td><td>${st.passed ? "Pass" : "Fail"}</td><td>${escapeHtml(st.details)}</td></tr>`;
      }
      html += `</tbody></table>`;
    } else {
      html += `<p><strong>Status:</strong> ${escapeHtml(ts.error || "Not found")}</p>
        <p>${escapeHtml(ts.repair_hint || "")}</p>`;
    }

    openModal("FFmpeg & ffprobe Tools", html, [
      {
        label: "Select tools folder…",
        cls: "btn",
        onClick: async () => {
          const dir = await dialog.open({ directory: true });
          if (dir) {
            await invoke("locate_ffmpeg_tools", { toolsDir: dir });
            openToolsModal();
          }
        },
      },
      {
        label: "Select ffmpeg binary…",
        cls: "btn",
        onClick: async () => {
          const ff = await dialog.open({ multiple: false });
          if (ff) {
            await invoke("locate_ffmpeg_tools", { ffmpegPath: ff });
            openToolsModal();
          }
        },
      },
      {
        label: "Select ffprobe binary…",
        cls: "btn",
        onClick: async () => {
          const fp = await dialog.open({ multiple: false });
          if (fp) {
            await invoke("locate_ffmpeg_tools", { ffprobePath: fp });
            openToolsModal();
          }
        },
      },
      {
        label: "Recheck",
        cls: "btn",
        onClick: async () => {
          await invoke("recheck_tools");
          openToolsModal();
        },
      },
      { label: "Done", cls: "btn btn-primary", onClick: closeModal },
    ]);
  }

  // Profiles Management Modal
  function openProfilesModal() {
    const q = activeQueue();
    if (!q) return;
    const wrap = document.createElement("div");
    wrap.innerHTML = `<p class="muted">Save or manage reusable batch workflow profiles. Deleting a profile leaves existing queues and jobs intact.</p>`;
    const list = document.createElement("div");
    list.style.display = "flex";
    list.style.flexDirection = "column";
    list.style.gap = "8px";
    for (const p of snapshot.profiles) {
      const row = document.createElement("div");
      row.style.display = "flex";
      row.style.justifyContent = "space-between";
      row.style.alignItems = "center";
      row.style.padding = "6px 8px";
      row.style.border = "1px solid var(--border)";
      row.style.borderRadius = "6px";
      row.innerHTML = `<span><strong>${escapeHtml(p.name)}</strong> <span class="muted">(${p.recipe.family === "remux" ? "Remux" : `${p.recipe.codec.toUpperCase()} CRF ${p.recipe.crf}`} · ${p.cleanup_policy})</span></span>`;
      const btns = document.createElement("div");
      btns.style.display = "flex";
      btns.style.gap = "4px";
      const upd = document.createElement("button");
      upd.className = "btn btn-sm";
      upd.textContent = "Update from queue";
      upd.onclick = async () => {
        await invoke("save_profile_from_queue", { queueId: q.id, name: p.name, existingProfileId: p.id });
        openProfilesModal();
      };
      const ren = document.createElement("button");
      ren.className = "btn btn-sm";
      ren.textContent = "Rename";
      ren.onclick = () => {
        openPromptModal("Rename Profile", "Profile name:", p.name, async (nm) => {
          await invoke("rename_profile", { profileId: p.id, newName: nm });
          await refreshSnapshot();
          openProfilesModal();
        });
      };
      const dup = document.createElement("button");
      dup.className = "btn btn-sm";
      dup.textContent = "Duplicate";
      dup.onclick = () => {
        openPromptModal("Duplicate Profile", "New profile name:", `${p.name} Copy`, async (nm) => {
          await invoke("duplicate_profile", { profileId: p.id, newName: nm });
          await refreshSnapshot();
          openProfilesModal();
        });
      };
      const del = document.createElement("button");
      del.className = "btn btn-sm btn-danger-outline";
      del.textContent = "Delete";
      del.onclick = async () => {
        await invoke("delete_profile", { profileId: p.id });
        await refreshSnapshot();
        openProfilesModal();
      };
      btns.append(upd, ren, dup, del);
      row.appendChild(btns);
      list.appendChild(row);
    }
    wrap.appendChild(list);

    openModalEl("Manage Workflow Profiles", wrap, [
      {
        label: "Save current queue as new profile…",
        cls: "btn btn-primary",
        onClick: () => {
          openPromptModal("Save Workflow Profile", "Profile name (e.g. Overnight archive):", "Overnight archive", async (nm) => {
            await invoke("save_profile_from_queue", { queueId: q.id, name: nm, existingProfileId: null });
            await refreshSnapshot();
            openProfilesModal();
          });
        },
      },
      { label: "Close", cls: "btn", onClick: closeModal },
    ]);
  }

  // Batch Consent Confirmation Dialog (Default to Keep!)
  function openCleanupConsentModal(q, resp) {
    let rowsHtml = "";
    for (const j of resp.consent_jobs.slice(0, 50)) {
      rowsHtml += `<li><span class="mono">${escapeHtml(j.source_rel_path)}</span> (${fmtBytes(j.input_bytes)})</li>`;
    }
    if (resp.consent_jobs.length > 50) {
      rowsHtml += `<li>…and ${resp.consent_jobs.length - 50} more</li>`;
    }
    const html = `<p><strong>Automatic Trash is enabled for this queue.</strong></p>
      <p>After each file passes full verification and A/V decode, the original will be moved to the OS Trash / Recycle Bin (never permanently deleted).</p>
      <p><strong>Count:</strong> ${resp.consent_jobs.length} files · <strong>Total size:</strong> ${fmtBytes(resp.consent_total_bytes)}<br>
      <strong>Destination:</strong> ${escapeHtml(resp.destination_root || "—")}</p>
      <ul style="max-height:180px;overflow:auto">${rowsHtml}</ul>`;

    openModal("Confirm Batch Source Cleanup", html, [
      {
        label: "Keep originals (recommended)",
        cls: "btn btn-primary",
        autofocus: true,
        onClick: async () => {
          await patchActiveQueueSettings((p) => {
            p.cleanup_policy = "keep_originals";
          });
          closeModal();
          await invoke("start_queue", { queueId: q.id, consentConfirmed: false });
        },
      },
      {
        label: `Move ${resp.consent_jobs.length} verified originals to Trash`,
        cls: "btn btn-danger",
        onClick: async () => {
          closeModal();
          await invoke("start_queue", { queueId: q.id, consentConfirmed: true });
        },
      },
    ]);
  }

  // Review Originals Modal (starts with NO items selected!)
  async function openReviewOriginalsModal(queueId) {
    const candidates = await invoke("get_cleanup_candidates", { queueId });
    const selectedIds = new Set();

    const draw = () => {
      const wrap = document.createElement("div");
      const totalSelectedBytes = candidates
        .filter((c) => selectedIds.has(c.job_id))
        .reduce((acc, c) => acc + c.input_bytes, 0);

      wrap.innerHTML = `<p>Review verified outputs before moving any original to the OS Trash / Recycle Bin. No files are selected by default.</p>`;
      const tbl = document.createElement("table");
      tbl.className = "table-simple";
      tbl.innerHTML = `<thead><tr><th>Trash</th><th>Source</th><th>Original → Output</th><th>Verification</th><th>Actions</th></tr></thead><tbody></tbody>`;
      const tbody = tbl.querySelector("tbody");

      for (const c of candidates) {
        const tr = document.createElement("tr");
        const canSelect = c.eligible && c.cleanup_outcome !== "trashed";
        const verNote = c.cleanup_outcome === "trashed"
          ? "Moved to Trash"
          : c.eligible
          ? `${c.codec_label}${c.full_decode_verified ? " · full decode OK" : " · will run full decode"}${c.has_approved_conversions ? " · approved conversions" : ""}`
          : c.ineligibility_reason || "Ineligible";

        tr.innerHTML = `<td><input type="checkbox" ${selectedIds.has(c.job_id) ? "checked" : ""} ${canSelect ? "" : "disabled"} /></td>
          <td><div class="mono" style="max-width:220px;overflow:hidden;text-overflow:ellipsis">${escapeHtml(c.source_path)}</div></td>
          <td>${fmtBytes(c.input_bytes)} → ${fmtBytes(c.output_bytes)}</td>
          <td>${escapeHtml(verNote)}</td>
          <td></td>`;
        const cb = tr.querySelector("input");
        cb.addEventListener("change", () => {
          if (cb.checked) selectedIds.add(c.job_id);
          else selectedIds.delete(c.job_id);
          draw();
        });
        const openBtn = document.createElement("button");
        openBtn.type = "button";
        openBtn.className = "btn btn-sm";
        openBtn.textContent = "Open output";
        openBtn.onclick = () => invoke("open_output_player", { path: c.output_path });
        tr.lastElementChild.appendChild(openBtn);
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
            const res = await invoke("trash_selected_originals", { jobIds: Array.from(selectedIds) });
            const errs = res.filter(([, r]) => r && r.Err).map(([, r]) => r.Err);
            if (errs.length > 0) {
              alert("Some files were kept:\n" + errs.join("\n"));
            }
            await refreshSnapshot();
            openReviewOriginalsModal(queueId);
          },
        },
      ]);
    };
    draw();
  }

  // 15-second Sample Preview & Comparison Modal
  async function openSamplePreviewModal(job) {
    const dur = job.probe?.format?.duration_secs || 60;
    const defaultStart = dur > 15 ? Math.min(dur * 0.25, Math.max(0, dur - 15)) : 0;
    let startInput = defaultStart;
    let zoom100 = false;
    let seekOffset = 0;
    let usingFrameFallback = false;

    const draw = async () => {
      const sp = snapshot.sample_preview && snapshot.sample_preview.job_id === job.id ? snapshot.sample_preview : null;
      const wrap = document.createElement("div");
      wrap.innerHTML = `
        <div style="display:flex;flex-wrap:wrap;align-items:center;gap:8px;margin-bottom:10px">
          <label>Start time (s):</label>
          <input id="smp-start" type="number" min="0" max="${Math.max(0, dur - 1).toFixed(1)}" step="1" value="${startInput.toFixed(1)}" style="width:80px" />
          <input id="smp-scrub" type="range" min="0" max="${Math.max(0, dur - 1).toFixed(1)}" step="0.5" value="${startInput.toFixed(1)}" style="flex:1" />
          <button id="smp-run" class="btn btn-primary btn-sm" type="button">Encode 15s sample</button>
        </div>`;

      if (sp) {
        if (sp.stale) {
          wrap.innerHTML += `<div class="banner banner-warn" style="margin-bottom:8px">Sample is stale (settings changed). Re-run to preview current settings.</div>`;
        }
        if (sp.status === "queued" || sp.status === "encoding") {
          wrap.innerHTML += `<p>${escapeHtml(sp.waiting_reason || "Encoding 15-second sample…")}</p>`;
        } else if (sp.status === "failed") {
          wrap.innerHTML += `<div class="banner banner-warn">Sample encode failed: ${escapeHtml(sp.error || "")}</div>`;
        } else if (sp.status === "ready") {
          const proj = sp.projected_bytes
            ? ` · Based on this scene: est. ${fmtBytes(sp.projected_bytes)} (${fmtDuration(sp.projected_secs)} runtime)`
            : "";
          wrap.innerHTML += `<p><strong>Sample ready:</strong> ${fmtBytes(sp.measured_bytes)} in ${sp.elapsed_secs?.toFixed(1)}s${proj}</p>
            <div style="display:flex;flex-wrap:wrap;align-items:center;gap:8px;margin-bottom:8px">
              <button id="cmp-play" class="btn btn-sm" type="button">Play / Pause both</button>
              <label>Seek (s): <span id="cmp-t-lbl">${seekOffset.toFixed(1)}</span></label>
              <input id="cmp-seek" type="range" min="0" max="${sp.duration_secs.toFixed(1)}" step="0.2" value="${seekOffset}" style="flex:1" />
              <button id="cmp-zoom" class="btn btn-sm" type="button">${zoom100 ? "Fit view" : "100% inspection"}</button>
              <button id="cmp-frames" class="btn btn-sm" type="button">Decode exact frames</button>
            </div>
            <div class="compare-grid">
              <div class="compare-pane ${zoom100 ? "zoom-100" : ""}">
                <div class="compare-label">Original</div>
                <video id="vid-orig" controls muted playsinline></video>
                <img id="img-orig" class="hidden" alt="Original frame" />
              </div>
              <div class="compare-pane ${zoom100 ? "zoom-100" : ""}">
                <div class="compare-label">Encoded (${escapeHtml(sp.recipe_snapshot.codec.toUpperCase())} CRF ${sp.recipe_snapshot.crf})</div>
                <video id="vid-enc" controls muted playsinline></video>
                <img id="img-enc" class="hidden" alt="Encoded frame" />
              </div>
            </div>`;
        }
      }

      const buttons = [];
      if (sp && sp.encoded_sample_path) {
        buttons.push({
          label: "Open sample in default player",
          cls: "btn",
          onClick: () => invoke("open_output_player", { path: sp.encoded_sample_path }),
        });
      }
      buttons.push({ label: "Close", cls: "btn btn-primary", onClick: closeModal });

      openModalEl(`15-Second Sample Preview — ${job.source_rel_path}`, wrap, buttons);

      const startEl = wrap.querySelector("#smp-start");
      const scrubEl = wrap.querySelector("#smp-scrub");
      startEl.onchange = () => { startInput = Number(startEl.value); scrubEl.value = startInput; };
      scrubEl.oninput = () => { startInput = Number(scrubEl.value); startEl.value = startInput; };
      wrap.querySelector("#smp-run").onclick = async () => {
        await invoke("request_sample_preview", { jobId: job.id, startSecs: startInput });
        await refreshSnapshot();
        draw();
      };

      if (sp && sp.status === "ready") {
        const vo = wrap.querySelector("#vid-orig");
        const ve = wrap.querySelector("#vid-enc");
        const io = wrap.querySelector("#img-orig");
        const ie = wrap.querySelector("#img-enc");
        const relOrig = `${job.id}/original_sample.mp4`;
        const relEnc = `${job.id}/encoded_sample.${sp.recipe_snapshot.container}`;
        const protoBase = navigator.userAgent.includes("Windows")
          ? "http://preview.localhost"
          : "preview://localhost";
        vo.src = `${protoBase}/${encodeURIComponent(relOrig)}`;
        ve.src = `${protoBase}/${encodeURIComponent(relEnc)}`;

        const loadExactFrames = async (t) => {
          usingFrameFallback = true;
          const [b1, b2] = await Promise.all([
            invoke("get_comparison_frame", { mediaPath: job.source_path, timestampSecs: sp.start_secs + t, fullResolution: zoom100 }),
            invoke("get_comparison_frame", { mediaPath: sp.encoded_sample_path, timestampSecs: t, fullResolution: zoom100 }),
          ]);
          io.src = URL.createObjectURL(new Blob([new Uint8Array(b1)], { type: "image/jpeg" }));
          ie.src = URL.createObjectURL(new Blob([new Uint8Array(b2)], { type: "image/jpeg" }));
          vo.classList.add("hidden");
          ve.classList.add("hidden");
          io.classList.remove("hidden");
          ie.classList.remove("hidden");
        };

        // Fall back automatically if browser webview cannot decode AV1/HEVC video element
        ve.onerror = () => loadExactFrames(seekOffset).catch(() => {});
        vo.onerror = () => loadExactFrames(seekOffset).catch(() => {});
        if (usingFrameFallback || zoom100) {
          loadExactFrames(seekOffset).catch(() => {});
        }

        wrap.querySelector("#cmp-play").onclick = () => {
          if (usingFrameFallback) {
            loadExactFrames(seekOffset).catch(() => {});
            return;
          }
          io.classList.add("hidden");
          ie.classList.add("hidden");
          vo.classList.remove("hidden");
          ve.classList.remove("hidden");
          if (vo.paused) { vo.play().catch(() => {}); ve.play().catch(() => {}); }
          else { vo.pause(); ve.pause(); }
        };
        wrap.querySelector("#cmp-seek").oninput = (e) => {
          seekOffset = Number(e.target.value);
          wrap.querySelector("#cmp-t-lbl").textContent = seekOffset.toFixed(1);
          vo.currentTime = seekOffset;
          ve.currentTime = seekOffset;
          if (usingFrameFallback || !io.classList.contains("hidden")) {
            loadExactFrames(seekOffset).catch(() => {});
          }
        };
        wrap.querySelector("#cmp-zoom").onclick = () => {
          zoom100 = !zoom100;
          usingFrameFallback = zoom100;
          draw();
        };
        wrap.querySelector("#cmp-frames").onclick = () => loadExactFrames(seekOffset);
      }
    };
    openSampleRefresh = draw;
    draw();
  }

  // Live Mid-Encode Preview Modal
  async function openLivePreviewModal(jobId) {
    currentLiveJobId = jobId;
    await invoke("start_live_preview", { jobId });
    await refreshSnapshot();

    const wrap = document.createElement("div");
    wrap.innerHTML = `
      <div id="lp-meta" style="margin-bottom:8px">Waiting for first encoded frame…</div>
      <img id="lp-img" class="live-preview-img" alt="Live encoded preview" />`;
    openModalEl("Live Encoded Preview (Silent)", wrap, [
      { label: "Hide preview (encode continues)", cls: "btn btn-primary", onClick: closeModal },
    ]);

    let prevBlobUrl = null;
    livePreviewTimer = setInterval(async () => {
      try {
        const snap = await invoke("get_snapshot");
        const li = snap.live_preview;
        const bytes = await invoke("get_live_preview_frame");
        const metaEl = document.getElementById("lp-meta");
        const imgEl = document.getElementById("lp-img");
        if (!metaEl || !imgEl) return;
        if (li) {
          if (li.limitation) {
            metaEl.textContent = li.limitation;
          } else if (li.waiting_first_frame) {
            metaEl.textContent = `Waiting for first encoded frame… (encoder lookahead · ${li.mode})`;
          } else {
            const pidTxt = li.worker_pid ? ` · Worker PID ${li.worker_pid}` : "";
            metaEl.textContent = `Media timestamp: ${fmtDuration(li.media_time_secs)} · Frame #${li.frame_seq} · ${li.mode}${pidTxt}`;
          }
        }
        if (bytes && bytes.byteLength > 0) {
          const url = URL.createObjectURL(new Blob([new Uint8Array(bytes)], { type: "image/jpeg" }));
          if (prevBlobUrl) URL.revokeObjectURL(prevBlobUrl);
          prevBlobUrl = url;
          imgEl.src = url;
        }
      } catch (_) {}
    }, 500);
  }

  function escapeHtml(s) {
    if (s === null || s === undefined) return "";
    return String(s)
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;");
  }

  // Wire Event Listeners
  $("btn-new-queue").onclick = () => invoke("create_queue_tab", { name: null });
  $("btn-tools-status").onclick = openToolsModal;
  $("btn-toggle-inspector").onclick = () => {
    const sp = $("side-panel");
    const collapsed = sp.classList.toggle("collapsed");
    $("btn-toggle-inspector").setAttribute("aria-expanded", String(!collapsed));
  };
  $("btn-toggle-advanced").onclick = () => {
    const adv = $("advanced-drawer");
    const hidden = adv.classList.toggle("hidden");
    $("btn-toggle-advanced").setAttribute("aria-expanded", String(!hidden));
  };

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

  $("chk-recursive").onchange = (e) => patchActiveQueueSettings((p) => { p.recursive_import = e.target.checked; });
  $("btn-choose-dest").onclick = async () => {
    const dir = await dialog.open({ directory: true });
    if (dir) patchActiveQueueSettings((p) => { p.destination_root = dir; });
  };

  $("btn-volume-reserve").onclick = () => {
    const q = activeQueue();
    const vol = findQueueVolumeInfo(q);
    if (!vol) return alert("Choose a destination folder first to configure its volume reserve.");
    const curGib = (vol.reserve_bytes / (1024 * 1024 * 1024)).toFixed(1);
    openPromptModal("Configure Volume Reserve", `Free-space reserve (GiB) for ${vol.mount_point}:`, curGib, async (val) => {
      const gib = Math.max(0, Number(val) || 0);
      await invoke("set_volume_reserve", {
        volumeId: vol.volume_id,
        mountHint: vol.mount_point,
        reserveBytes: Math.round(gib * 1024 * 1024 * 1024),
      });
    });
  };

  $("btn-counts-filter").onclick = () => {
    filterNeedsReview = !filterNeedsReview;
    renderQueueList();
    renderToolbar();
  };

  $("btn-start-queue").onclick = async () => {
    const q = activeQueue();
    if (!q) return;
    const jobs = activeQueueJobs();
    const isResume = q.is_paused || jobs.some((j) => j.status === "interrupted");
    const resp = await invoke(isResume ? "resume_queue" : "start_queue", {
      queueId: q.id,
      consentConfirmed: null,
    });
    if (resp && resp.requires_cleanup_consent) {
      openCleanupConsentModal(q, resp);
    }
  };
  $("btn-pause-after").onclick = () => invoke("pause_after_current", { queueId: activeQueue().id });
  $("btn-cancel-current").onclick = () => invoke("cancel_current_file", { jobId: null });
  $("btn-retry-failed").onclick = async () => {
    const jobs = activeQueueJobs();
    const checkedRetry = jobs
      .filter((j) => checkedJobIds.has(j.id) && ["failed", "canceled", "interrupted", "needs_attention"].includes(j.status))
      .map((j) => j.id);
    const ids = checkedRetry.length > 0
      ? checkedRetry
      : jobs.filter((j) => ["failed", "canceled", "interrupted", "needs_attention"].includes(j.status)).map((j) => j.id);
    try {
      await invoke("retry_jobs", { jobIds: ids });
    } catch (err) {
      alert(err);
    }
  };
  $("btn-clear-completed").onclick = () => {
    const ids = activeQueueJobs()
      .filter((j) => ["completed", "below_savings_target", "canceled"].includes(j.status))
      .map((j) => j.id);
    invoke("remove_pending_jobs", { jobIds: ids });
  };

  // Config Controls
  $("sel-profile").onchange = (e) => {
    if (e.target.value) {
      invoke("select_profile_for_queue", { queueId: activeQueue().id, profileId: e.target.value });
    }
  };
  $("btn-profile-menu").onclick = openProfilesModal;
  $("btn-apply-waiting").onclick = () => {
    const q = activeQueue();
    const checkedWaiting = activeQueueJobs()
      .filter((j) => checkedJobIds.has(j.id) && ["queued", "ready", "needs_attention", "interrupted", "failed", "canceled"].includes(j.status))
      .map((j) => j.id);
    invoke("apply_queue_settings_to_waiting", {
      queueId: q.id,
      jobIds: checkedWaiting.length > 0 ? checkedWaiting : null,
    });
  };

  $("sel-family").onchange = (e) => patchActiveQueueSettings((p) => { p.recipe.family = e.target.value; applyCodecDefaults(p.recipe); });
  $("sel-variant").onchange = (e) => patchActiveQueueSettings((p) => { p.recipe.variant = e.target.value; applyCodecDefaults(p.recipe); });
  $("sel-codec").onchange = (e) => patchActiveQueueSettings((p) => { p.recipe.codec = e.target.value; applyCodecDefaults(p.recipe); });
  $("sel-container").onchange = (e) => patchActiveQueueSettings((p) => { p.recipe.container = e.target.value; });
  $("btn-reset-preset").onclick = () => patchActiveQueueSettings((p) => applyCodecDefaults(p.recipe));

  $("inp-crf").onchange = (e) => patchActiveQueueSettings((p) => { p.recipe.crf = Number(e.target.value); });
  $("sel-effort").onchange = (e) => patchActiveQueueSettings((p) => { p.recipe.effort = e.target.value; });
  $("sel-priority").onchange = (e) => patchActiveQueueSettings((p) => { p.priority = e.target.value; });
  $("sel-error-policy").onchange = (e) => patchActiveQueueSettings((p) => { p.error_policy = e.target.value; });
  $("sel-cleanup-policy").onchange = (e) => patchActiveQueueSettings((p) => { p.cleanup_policy = e.target.value; });
  $("chk-keep-awake").onchange = (e) => patchActiveQueueSettings((p) => { p.keep_awake = e.target.checked; });
  $("chk-full-decode").onchange = (e) => patchActiveQueueSettings((p) => { p.full_decode_verify = e.target.checked; });
  $("chk-min-savings").onchange = (e) => patchActiveQueueSettings((p) => { p.min_savings_enabled = e.target.checked; });
  $("inp-min-savings-pct").onchange = (e) => patchActiveQueueSettings((p) => { p.min_savings_pct = Number(e.target.value); });

  $("btn-save-named-preset").onclick = () => {
    openPromptModal("Save Encode Preset", "Preset name:", "", async (nm) => {
      await invoke("save_named_preset", { name: nm, recipe: activeQueue().recipe });
    });
  };
  $("btn-delete-named-preset").onclick = async () => {
    const id = $("sel-named-preset").value;
    if (id) {
      await invoke("delete_named_preset", { id });
    }
  };
  $("sel-named-preset").onchange = (e) => {
    const np = snapshot.named_presets.find((x) => x.id === e.target.value);
    $("btn-delete-named-preset").classList.toggle("hidden", !e.target.value);
    if (np) patchActiveQueueSettings((p) => { p.recipe = { ...np.recipe }; });
  };
  $("btn-preview-15s").onclick = () => {
    const jobs = activeQueueJobs();
    const job = jobs.find((j) => j.id === selectedJobId) || jobs[0];
    if (job) openSamplePreviewModal(job);
  };

  // Tauri Native Drag & Drop
  tauriWindow.getCurrentWindow().onDragDropEvent((ev) => {
    if (ev.payload.type === "over") {
      $("empty-drop-state").classList.add("drag-over");
    } else if (ev.payload.type === "leave") {
      $("empty-drop-state").classList.remove("drag-over");
    } else if (ev.payload.type === "drop") {
      $("empty-drop-state").classList.remove("drag-over");
      const q = activeQueue();
      if (q && q.tab_type === "repair" && ev.payload.paths && ev.payload.paths.length > 0) {
        const s = activeRepairSession();
        saveRepairConfig({ input_path: ev.payload.paths[0], reference_paths: s.reference_paths, output_path: s.output_path });
      } else {
        startImportFlow(ev.payload.paths);
      }
    }
  });

  // Closing window during active work offers Keep running (minimize) or Stop and quit
  tauriWindow.getCurrentWindow().onCloseRequested(async (ev) => {
    const busy = await invoke("check_close_status");
    if (!busy) return;
    ev.preventDefault();
    openModal("Encoding Queue Is Active", "<p>A batch is currently running. Do you want to minimize the window and keep running in the background, or stop active work and quit?</p>", [
      {
        label: "Keep running (minimize)",
        cls: "btn btn-primary",
        autofocus: true,
        onClick: async () => {
          closeModal();
          await tauriWindow.getCurrentWindow().minimize();
        },
      },
      {
        label: "Stop and quit",
        cls: "btn btn-danger",
        onClick: async () => {
          closeModal();
          await invoke("stop_and_quit");
        },
      },
    ]);
  });

  // Recheck volumes on focus / wake from sleep
  window.addEventListener("focus", () => {
    invoke("recheck_volumes").catch(() => {});
  });
  document.addEventListener("visibilitychange", () => {
    if (!document.hidden) {
      invoke("recheck_volumes").catch(() => {});
    }
  });

  // Listen to backend compact events
  event.listen("snapshot_changed", () => refreshSnapshot());
  event.listen("repair_progress", (ev) => {
    const q = activeQueue();
    if (!q || !ev.payload || ev.payload.queue_id !== q.id) return;
    renderRepairProgress(ev.payload.progress);
  });
  event.listen("job_progress", (ev) => {
    if (!snapshot) return;
    snapshot.active_progress = ev.payload;
    renderQueueList();
  });
  event.listen("sample_preview_updated", (ev) => {
    if (!snapshot) return;
    snapshot.sample_preview = ev.payload;
    if (openSampleRefresh) openSampleRefresh();
  });

  function activeRepairSession() {
    const q = activeQueue();
    const empty = {
      queue_id: q ? q.id : "",
      input_path: "",
      reference_paths: [],
      output_path: null,
      video_only: false,
      deep_scan: true,
      timeout_secs: 300,
      status: "idle",
      diagnosis: null,
      report: null,
      error: null,
      preview_sheet_filename: null,
    };
    if (!q || !snapshot || !snapshot.repair_sessions) return empty;
    return snapshot.repair_sessions.find((s) => s.queue_id === q.id) || empty;
  }

  async function saveRepairConfig(overrides) {
    const q = activeQueue();
    if (!q) return;
    const cur = activeRepairSession();
    const merged = {
      queueId: q.id,
      inputPath: overrides.input_path !== undefined ? overrides.input_path : cur.input_path,
      referencePaths: overrides.reference_paths !== undefined ? overrides.reference_paths : cur.reference_paths,
      outputPath: overrides.output_path !== undefined ? overrides.output_path : cur.output_path,
      videoOnly: $("chk-repair-video-only").checked,
      deepScan: $("chk-repair-deep").checked,
      timeoutSecs: Math.max(15, Number($("inp-repair-timeout").value) || 300),
    };
    await invoke("update_repair_session", merged);
  }

  function renderRepairProgress(p) {
    const box = $("repair-progress-box");
    if (!p) {
      box.classList.add("hidden");
      return;
    }
    box.classList.remove("hidden");
    const pct = p.total_bytes > 0 ? Math.min(100, (p.bytes_scanned / p.total_bytes) * 100) : 0;
    $("lbl-repair-stage").textContent = `${p.current_strategy}`;
    $("lbl-repair-metrics").textContent = `${pct.toFixed(1)}% · ${fmtBytes(p.bytes_scanned)}/${fmtBytes(p.total_bytes)} · ${p.recovered_frames} frames · ${p.elapsed_secs.toFixed(1)}s`;
    $("bar-repair-progress").style.width = `${pct.toFixed(1)}%`;
  }

  function esc(s) {
    return String(s ?? "").replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
  }

  function renderRepairWorkspace() {
    const s = activeRepairSession();
    const busy = s.status === "diagnosing" || s.status === "recovering";
    $("inp-repair-input").value = s.input_path || "";
    $("inp-repair-output").value = s.output_path || "";
    $("inp-repair-timeout").value = s.timeout_secs || 300;
    $("chk-repair-deep").checked = !!s.deep_scan;
    $("chk-repair-video-only").checked = !!s.video_only;
    $("btn-repair-diagnose").disabled = busy || !s.input_path;
    $("btn-repair-start").disabled = busy || !s.input_path;

    const chips = $("repair-refs-chips");
    chips.innerHTML = "";
    if (!s.reference_paths || s.reference_paths.length === 0) {
      const hint = document.createElement("span");
      hint.className = "muted";
      hint.textContent = "No reference files attached (add healthy files from same camera/series if moov is missing).";
      chips.appendChild(hint);
    } else {
      for (const rp of s.reference_paths) {
        const chip = document.createElement("span");
        chip.className = "chip";
        const name = rp.split(/[/\\]/).pop();
        chip.innerHTML = `<span title="${esc(rp)}">${esc(name)}</span>`;
        const rm = document.createElement("button");
        rm.type = "button";
        rm.textContent = "×";
        rm.title = "Remove reference";
        rm.onclick = () => saveRepairConfig({ reference_paths: s.reference_paths.filter((x) => x !== rp) });
        chip.appendChild(rm);
        chips.appendChild(chip);
      }
    }

    if (!busy) {
      $("repair-progress-box").classList.add("hidden");
    } else {
      $("repair-progress-box").classList.remove("hidden");
    }

    const resEl = $("repair-results");
    if (!s.diagnosis && !s.report && !s.error) {
      resEl.classList.add("hidden");
      resEl.innerHTML = "";
      return;
    }
    resEl.classList.remove("hidden");

    let html = "";
    if (s.error) {
      html += `<div class="repair-card"><strong style="color:var(--err)">Error:</strong> ${esc(s.error)}</div>`;
    }

    const rep = s.report;
    const diag = rep ? rep.diagnosis : s.diagnosis;

    if (rep) {
      const badgeCls = rep.overall_status === "recovered" ? "ok" : rep.overall_status === "partial" ? "warn" : "err";
      html += `
        <div class="repair-card">
          <div class="inline-row" style="justify-content:space-between; flex-wrap:wrap; gap:8px;">
            <div>
              <span class="dot ${badgeCls}" style="display:inline-block;margin-right:6px;"></span>
              <strong>Status: ${esc(rep.overall_status.toUpperCase())}</strong>
              <span class="muted"> · Original input SHA-256 ${rep.input_untouched_verified ? "Verified Unchanged ✓" : "CHECK FAILED"} · Elapsed ${rep.elapsed_secs.toFixed(1)}s</span>
              <p style="margin:6px 0 0;">${esc(rep.honest_summary)}</p>
            </div>
            <div class="inline-row">
              ${rep.output_path ? `<button class="btn btn-secondary btn-sm" id="btn-repair-reveal" type="button">Reveal output</button>` : ""}
              ${rep.output_path ? `<button class="btn btn-primary btn-sm" id="btn-repair-open" type="button">Open in player</button>` : ""}
              <button class="btn btn-ghost btn-sm" id="btn-repair-export-txt" type="button">Export TXT</button>
              <button class="btn btn-ghost btn-sm" id="btn-repair-export-json" type="button">Export JSON</button>
            </div>
          </div>
        </div>
      `;
    }

    if (diag) {
      const atomsRows = (diag.atoms || []).map((a) => `
        <tr>
          <td class="mono">${esc(a.tag)}</td>
          <td class="mono">${a.offset}</td>
          <td class="mono">${fmtBytes(a.declared_size)}</td>
          <td class="mono">${a.physical_end}</td>
          <td>${a.truncated ? `<span style="color:var(--warn)">Truncated</span>` : "Intact"}</td>
        </tr>
      `).join("");
      html += `
        <div class="repair-card">
          <h3 style="margin:0 0 8px;">Structure Diagnosis (${esc(diag.container_kind)})</h3>
          <div class="mono muted" style="margin-bottom:8px;">SHA-256: ${esc(diag.sha256)} · ${fmtBytes(diag.file_size)}</div>
          <ul class="provenance-list">
            ${(diag.summary_lines || []).map((l) => `<li>${esc(l)}</li>`).join("")}
            ${(diag.anomalies || []).map((a) => `<li style="color:var(--warn)">${esc(a)}</li>`).join("")}
          </ul>
          ${atomsRows ? `<table class="table-simple"><thead><tr><th>Atom</th><th>Offset</th><th>Declared</th><th>Physical End</th><th>State</th></tr></thead><tbody>${atomsRows}</tbody></table>` : ""}
        </div>
      `;
    }

    if (rep) {
      const candRows = (rep.candidates || []).map((c) => `
        <tr>
          <td><strong>${esc(c.strategy_name)}</strong>${c.rejection_reason ? `<div class="muted" style="font-size:11px">${esc(c.rejection_reason)}</div>` : ""}</td>
          <td>${c.succeeded ? `<span style="color:var(--ok);font-weight:600">PASS</span>` : `<span style="color:var(--err)">FAIL</span>`}</td>
          <td class="mono">${c.score.toFixed(1)}</td>
          <td class="mono">${c.decoded_video_frames}</td>
          <td class="mono">${c.idr_frames}</td>
          <td class="mono">${c.decode_error_count}</td>
          <td class="mono">${c.recovered_duration_secs != null ? c.recovered_duration_secs.toFixed(2) + "s" : "—"}</td>
          <td class="mono">${c.audio_duration_secs != null ? c.audio_duration_secs.toFixed(2) + "s" : "Video-only"}</td>
        </tr>
      `).join("");

      html += `
        <div class="repair-card">
          <h3 style="margin:0 0 8px;">Recovery Candidates Evaluated (${(rep.candidates || []).length})</h3>
          <table class="table-simple">
            <thead><tr><th>Strategy</th><th>Valid</th><th>Score</th><th>Frames</th><th>IDRs</th><th>Decode Errs</th><th>Video Dur</th><th>Audio</th></tr></thead>
            <tbody>${candRows}</tbody>
          </table>
        </div>
        <div class="repair-two-col">
          <div class="repair-card">
            <h3 style="margin:0 0 8px;">Data Provenance &amp; VR Notes</h3>
            <strong>Recovered Data</strong>
            <ul class="provenance-list">${(rep.provenance.recovered_data || []).map((x) => `<li>${esc(x)}</li>`).join("")}</ul>
            <strong>Inferred Metadata</strong>
            <ul class="provenance-list">${(rep.provenance.inferred_metadata || []).map((x) => `<li>${esc(x)}</li>`).join("")}</ul>
            <strong>Irrecoverable / Omitted</strong>
            <ul class="provenance-list">${(rep.provenance.irrecoverable_or_omitted || []).map((x) => `<li>${esc(x)}</li>`).join("")}</ul>
            ${rep.vr_status && rep.vr_status.limitation_note ? `<p style="color:var(--warn);font-size:12px;"><strong>VR Note:</strong> ${esc(rep.vr_status.limitation_note)}</p>` : ""}
          </div>
          <div class="repair-card">
            <h3 style="margin:0 0 8px;">Contact Sheet Thumbnail Verification</h3>
            ${s.preview_sheet_filename ? `<img class="contact-sheet-img" src="preview://${encodeURIComponent(s.preview_sheet_filename)}?t=${s.updated_at}" alt="Recovered frames contact sheet" />` : `<p class="muted">No contact sheet generated.</p>`}
          </div>
        </div>
      `;
    }

    resEl.innerHTML = html;

    if (rep && rep.output_path) {
      const btnRev = $("btn-repair-reveal");
      const btnOpen = $("btn-repair-open");
      if (btnRev) btnRev.onclick = () => invoke("reveal_output", { path: rep.output_path });
      if (btnOpen) btnOpen.onclick = () => invoke("open_output_player", { path: rep.output_path });
    }
    const btnTxt = $("btn-repair-export-txt");
    const btnJson = $("btn-repair-export-json");
    if (btnTxt) btnTxt.onclick = async () => {
      const dst = await dialog.save({ defaultPath: "recovery_report.txt" });
      if (dst) await invoke("export_repair_report", { queueId: activeQueue().id, destPath: dst, format: "txt" });
    };
    if (btnJson) btnJson.onclick = async () => {
      const dst = await dialog.save({ defaultPath: "recovery_report.json" });
      if (dst) await invoke("export_repair_report", { queueId: activeQueue().id, destPath: dst, format: "json" });
    };
  }

  // Repair Tab Actions
  $("btn-repair-pick-input").onclick = async () => {
    const picked = await dialog.open({ multiple: false, filters: [{ name: "Video", extensions: ["mp4", "mov", "m4v"] }] });
    if (picked) await saveRepairConfig({ input_path: picked });
  };
  $("btn-repair-add-refs").onclick = async () => {
    const picked = await dialog.open({ multiple: true, filters: [{ name: "Video", extensions: ["mp4", "mov", "m4v", "mkv"] }] });
    if (picked) {
      const list = Array.isArray(picked) ? picked : [picked];
      const cur = activeRepairSession().reference_paths || [];
      const merged = Array.from(new Set([...cur, ...list]));
      await saveRepairConfig({ reference_paths: merged });
    }
  };
  $("btn-repair-pick-output").onclick = async () => {
    const cur = activeRepairSession();
    const picked = await dialog.save({ defaultPath: cur.output_path || "recovered.mp4" });
    if (picked) await saveRepairConfig({ output_path: picked });
  };
  $("chk-repair-deep").onchange = () => saveRepairConfig({});
  $("chk-repair-video-only").onchange = () => saveRepairConfig({});
  $("inp-repair-timeout").onchange = () => saveRepairConfig({});
  $("btn-repair-diagnose").onclick = async () => {
    try {
      await invoke("run_repair_diagnosis", { queueId: activeQueue().id });
    } catch (err) {
      alert(err);
    }
  };
  $("btn-repair-start").onclick = async () => {
    try {
      await invoke("start_video_repair", { queueId: activeQueue().id });
    } catch (err) {
      alert(err);
    }
  };

  refreshSnapshot();
})();
