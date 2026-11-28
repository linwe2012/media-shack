(() => {
  const N = ({ smoke: 1000, "10k": 10000, "50k": 50000 })[window.__STRESS_SIZE__] || 1000;
  const TOTAL_BYTES = 1_000_000_000_000;
  const recipe = { schema_version: 1, family: "archive", variant: "balanced", codec: "hevc", container: "mp4", crf: 20, effort: "slow", tune: null, ten_bit: true, aq_mode: null, is_custom: false, preset_name: "Stress fixture" };
  const makeQueue = (id, name, tab_order) => ({ id, name, tab_order, tab_type: "encode", profile_id: "stress-profile", profile_name: "Stress fixture", recipe: { ...recipe }, priority: "background", recursive_import: true, destination_root: `/mock/output/${id}`, keep_awake: false, error_policy: "continue", min_savings_enabled: false, min_savings_pct: 0, cleanup_policy: "keep_originals", full_decode_verify: false, is_running: false, is_paused: false, pause_after_current: false, waiting_reason: null });
  const queues = [makeQueue("queue-main", "Primary · 1TB fixture", 0), makeQueue("queue-shared", "Shared source sample", 1)];
  const jobs = [];
  const base = Math.floor(TOTAL_BYTES / N), remainder = TOTAL_BYTES - base * N;
  for (let i = 0; i < N; i++) {
    const index = String(i + 1).padStart(6, "0");
    const file = i === N - 1 ? `SENTINEL-LAST-FILE-${index}.mkv` : `clip-${index}.mkv`;
    const rel = `Archive/Batch-${String(Math.floor(i / 1000) + 1).padStart(3, "0")}/Set-${String(Math.floor(i / 100) % 10).padStart(2, "0")}/${file}`;
    jobs.push({ id: `stress-${index}`, queue_id: "queue-main", batch_id: "stress-batch", position: i, source_path: `/mock/media/${rel}`, source_rel_path: rel, source_root: "/mock/media", source_fingerprint: null, destination_path: `/mock/output/queue-main/${rel}`, dest_volume_id: "mock-volume", recipe: { ...recipe }, compat_overrides: {}, status: "queued", status_reason: null, probe: { streams: [{ codec_type: "video", codec_name: "H264", width: 1920, height: 1080, bit_rate: 12000000, disposition: {} }], format: { duration_secs: 720, bit_rate: 12000000 } }, plan: null, input_bytes: base + (i < remainder ? 1 : 0), output_bytes: null, output_fingerprint: null, error_stage: null, error_message: null, diagnostics: null, cleanup_policy: "keep_originals", cleanup_outcome: "not_attempted", cleanup_error: null, consent_id: null, full_decode_verified: false, min_savings_enabled: false, min_savings_pct: 0, tool_versions: null, current_attempt_id: null, created_at: 1_700_000_000 + i, updated_at: 1_700_000_000 + i, completed_at: null });
  }
  // A second queue references the same first 25 source records; it adds no physical bytes.
  for (let i = 0; i < Math.min(25, N); i++) jobs.push({ ...jobs[i], id: `shared-${String(i + 1).padStart(3, "0")}`, queue_id: "queue-shared", position: i, destination_path: `/mock/output/queue-shared/${jobs[i].source_rel_path}` });
  const snapshot = { queues, active_queue_id: "queue-main", jobs, repair_sessions: [], tab_meta: { "queue-main": {}, "queue-shared": {} }, job_decisions: {}, protected_sources: [], tab_dedupe_groups: {}, tab_repair_items: {}, hash_cache_stats: { entry_count: 0, total_hashed_bytes: 0, cache_bytes_estimate: 0 }, profiles: [{ id: "stress-profile", name: "Stress fixture", recipe: { ...recipe }, priority: "background", recursive_import: true, destination_root: "/mock/output", keep_awake: false, error_policy: "continue", min_savings_enabled: false, min_savings_pct: 0, cleanup_policy: "keep_originals", full_decode_verify: false, created_at: 1, updated_at: 1 }], named_presets: [], tools: { discovered: true, capabilities: { ffmpeg_path: "MOCK", ffprobe_path: "MOCK", ffmpeg_version: "mock", ffprobe_version: "mock", has_libx265: true, has_libsvtav1: true, has_aac: true, has_matroska: true, has_mp4: true, has_mov_text: true, available_codecs: ["hevc", "av1"], smoke_tests: [], checked_at: 1, identity_key: "mock" }, error: null, repair_hint: null }, volumes: [{ volume_id: "mock-volume", mount_point: "/mock", free_bytes: 4_000_000_000_000, total_bytes: 5_000_000_000_000, reserve_bytes: 0, blocked_reason: null }], morning_summary: { queue_id: "queue-main", completed_count: 0, failed_count: 0, needs_attention_count: 0, below_savings_count: 0, canceled_count: 0, interrupted_count: 0, total_input_bytes: 0, total_output_bytes: 0, trashed_count: 0, kept_count: 0, cleanup_failed_count: 0, cleanup_uncertain_count: 0, recoverable_bytes_by_volume: {} }, active_progress: null, sample_preview: null, live_preview: null, power_assertion_active: false, scheduler_busy_reason: null };
  const listeners = new Map();
  const fire = (name, payload) => (listeners.get(name) || []).forEach(fn => { try { fn({ payload }); } catch (e) { console.error(e); } });
  let churnTimer = null, churnTick = 0;
  const clone = (x) => structuredClone(x);
  async function invoke(command, args = {}) {
    if (command === "get_snapshot") return snapshot;
    if (command === "select_queue_tab") { snapshot.active_queue_id = args.queueId; fire("snapshot_changed"); return true; }
    if (command === "update_queue_settings") { const q = queues.find(x => x.id === args.queueId); if (q) Object.assign(q, args); fire("snapshot_changed"); return true; }
    if (command === "start_queue" || command === "resume_queue") { const q = queues.find(x => x.id === args.queueId); if (q) q.is_running = true; return { ok: true }; }
    if (command === "create_queue_tab") { const q = makeQueue(`queue-${queues.length + 1}`, `Mock queue ${queues.length + 1}`, queues.length); queues.push(q); snapshot.active_queue_id = q.id; fire("snapshot_changed"); return q; }
    if (command === "rename_queue_tab") { const q = queues.find(x => x.id === args.queueId); if (q) q.name = args.name; fire("snapshot_changed"); return true; }
    if (command === "set_queue_tab_type") { const q = queues.find(x => x.id === args.queueId); if (q) q.tab_type = args.tabType; fire("snapshot_changed"); return true; }
    if (command === "delete_queue_tab") { const i = queues.findIndex(x => x.id === args.queueId); if (i >= 0 && queues.length > 1) queues.splice(i, 1); snapshot.active_queue_id = queues[0].id; fire("snapshot_changed"); return true; }
    if (command === "locate_ffmpeg_tools" || command === "recheck_tools") return snapshot.tools;
    if (command === "get_cleanup_candidates") return [];
    if (command === "check_close_status") return false;
    // Destructive commands deliberately remain in-memory no-ops.
    return { ok: true, count: 0, items: [] };
  }
  window.__TAURI__ = {
    core: { invoke }, event: { listen: async (name, fn) => { if (!listeners.has(name)) listeners.set(name, []); listeners.get(name).push(fn); return () => {}; } },
    dialog: { open: async () => null, save: async () => null, message: async () => {}, ask: async () => false },
    window: { getCurrentWindow: () => ({ onDragDropEvent: async () => () => {}, onCloseRequested: async () => () => {}, close: async () => {}, minimize: async () => {} }) },
  };
  const timing = { loadStarted: performance.now(), loadReady: null, searches: [] };
  const metric = () => ({ fileCount: N, bytes: TOTAL_BYTES, visibleRows: document.querySelectorAll("#queue-rows tbody tr.tc-row").length, renderedRows: document.querySelectorAll("#queue-rows tbody tr.tc-row").length, totalJobs: jobs.length, uniquePhysicalBytes: TOTAL_BYTES, activeQueue: snapshot.active_queue_id, status: "ready", dataset: window.__STRESS_SIZE__, loadMs: timing.loadReady ?? performance.now() - timing.loadStarted, lastSearchMs: timing.searches.at(-1)?.ms ?? null, searchCount: timing.searches.length });
  const find = (needle) => {
    const input = document.getElementById("enc-search-input"); if (!input) return { found: false, reason: "search input unavailable" };
    const started = performance.now(); input.value = needle; input.dispatchEvent(new Event("input", { bubbles: true }));
    const rows = [...document.querySelectorAll("#queue-rows tbody tr.tc-row")];
    const row = rows.find(r => r.textContent.includes(needle) || r.getAttribute("data-job-id")?.includes(needle));
    if (row) { row.scrollIntoView({ block: "center" }); row.click(); }
    const result = { found: !!row, visibleRows: rows.length, text: row?.textContent.slice(0, 160) || null, needle, ms: performance.now() - started };
    timing.searches.push(result); return result;
  };
  const setScenario = (name) => {
    if (name !== "progress") return false;
    if (churnTimer) clearInterval(churnTimer);
    churnTimer = setInterval(() => {
      churnTick++;
      const j = jobs[(churnTick * 37) % N]; j.status = "encoding";
      const p = { job_id: j.id, queue_id: j.queue_id, status: "encoding", out_time_secs: churnTick % 720, duration_secs: 720, speed: 1.25, fps: 30, bytes_written: churnTick * 1024, eta_secs: 500, est_final_ratio: 0.6, elapsed_secs: churnTick };
      snapshot.active_progress = p; fire("job_progress", p);
    }, 100);
    return true;
  };
  window.__stressHarness = { ready: Promise.resolve(true), metrics: metric, setScenario, find, search: (s) => find(s), stop: () => { if (churnTimer) clearInterval(churnTimer); churnTimer = null; }, data: { queues, jobs, snapshot } };
  requestAnimationFrame(() => requestAnimationFrame(() => { timing.loadReady = performance.now() - timing.loadStarted; }));
})();
