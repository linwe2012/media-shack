# Architecture Decision Record (ADR): MediaShack Unified Media Management and Archival Platform

## Status
Accepted — Extending MediaShack from a batch video encoder/remuxer into a unified local-first media management and archival platform.

## Context
MediaShack currently ships a Tauri 2 + Rust + vanilla HTML/CSS/JS desktop application with:
- Persistent SQLite database (`db.rs`) with versioned migrations, WAL mode, and foreign keys.
- Platform-abstracted file identity tracking (`platform/common.rs`, `windows.rs`, `macos.rs`) using volume IDs, OS file IDs/inodes, and head/tail content hashes.
- FFprobe/FFmpeg discovery, capability caching, stream planning, supervised execution, output verification, and source-safe Trash cleanup (`tools.rs`, `planner.rs`, `runner.rs`, `verify.rs`, `cleanup.rs`).
- Multi-tab overnight queue scheduler (`scheduler.rs`) with atomic same-volume publication and crash recovery.

To fulfill Implementation Specification v1.0 ("Scan once, cache everything, and reuse analysis across features") without rewriting the application or breaking existing transcoding workflows, we adopt the following architectural decisions.

## Decisions

### ADR-001: Preserve Tauri 2 + Rust Core + Vanilla UI Stack
- **Decision**: Keep Tauri 2, Rust backend, and static `ui/` (`index.html`, `styles.css`, `app.js`) with no Node.js build step required.
- **Rationale**: Keeps binary footprint small, startup fast, memory low (<16GB Mac target), and preserves existing IPC contracts and tests.

### ADR-002: Unified Persistent Media Catalog in SQLite (Migrations v3+)
- **Decision**: Extend `media_shack.db` via transactional migrations (`v3+`) with normalized catalog tables (`library_roots`, `volumes`, `media_assets`, `media_files`, `file_locations`, `media_relationships`, `tags`, `media_tags`, `collections`, `fingerprints`, `similarity_edges`, `analysis_results`, `media_embeddings`, `analysis_jobs`, `operation_history`, `integrity_results`, `metadata_sync_state`, `ocr_detections`, `face_groups`, `face_observations`) plus `media_fts` (SQLite FTS5).
- **Identity Model**: Separate `media_assets` (logical asset UUID) from `media_files` (physical file instance keyed by `volume_id + file_id + blake3/partial_hash`) and `file_locations` (current/historical paths). Hard links share a `media_files` row; renames/moves update `file_locations` without losing tags, embeddings, or analysis. Disconnected volumes retain rows marked `is_online = 0`.
- **Artifact Storage**: Keep SQLite lean; store thumbnails, sampled frames, and face crops in `<app_data_dir>/analysis_cache/` indexed by content identity + algorithm/model version with configurable size eviction.

### ADR-003: Hashing & Multi-Method Duplicate/Similarity Detection in Pure Rust + FFmpeg
- **Decision**: Use `blake3` for fast exact duplicate detection and local integrity verification, `sha2` (SHA-256) for archival checksum manifests, FFmpeg stream/packet MD5 (`-f streamhash`) for container-independent encoded-stream equivalence, and pure-Rust `image` + 2D-DCT pHash / dHash + temporal fingerprint sequences for perceptual video similarity.
- **Scalability**: Avoid O(N²) comparisons via size bucketing (exact), stream-hash indexing (equivalent), and multi-band LSH / duration-window candidate retrieval (perceptual), followed by offset-voting / dynamic time warping (DTW) temporal alignment for partial clips and overlapping edits.

### ADR-004: Metadata Interoperability (SQLite Primary, Opt-In OS/Embedded/Sidecars)
- **Decision**: SQLite remains the source of truth. Optional sync targets:
  - **Windows**: Native `IPropertyStore` (`PKEY_Keywords` / `System.Keywords`) via Windows Shell Property System COM APIs, probing handler writability before writing.
  - **macOS**: Native Finder tags via `com.apple.metadata:_kMDItemUserTags` xattr plist, detecting unsupported filesystems.
  - **Embedded**: External `exiftool` (with FFmpeg lossless `-c copy` metadata remux fallback) never re-encoding streams, automatically invalidating/updating content hashes afterward.
  - **Portable Sidecars**: JSON (`.mediashack.json`) and XMP (`.xmp`) sidecar import/export with explicit conflict detection and resolution policies (`prefer_db`, `prefer_external`, `merge`, `manual`).

### ADR-005: Shared Resource-Aware Task Manager & Non-Destructive Repair/Transcode Integration
- **Decision**: Introduce a unified `TaskManager` backed by `analysis_jobs` with configurable CPU, GPU, disk I/O (HDD vs SSD concurrency serialization), memory, and FFmpeg process budgets. Analysis tasks yield/throttle when encoding is active.
- **Repair**: Build a modular `repair` subsystem coordinating MP4/MOV atom inspection (`ftyp`/`moov`/`mdat`), structural/decode diagnostics, FFmpeg bitstream/remux recovery, and reference-file (`untrunc`-style) MP4 reconstruction, verifying every repaired candidate by full decoding before marking successful and recording `media_relationships`.
- **Transcode Integration**: Any catalog selection or virtual collection can enqueue transcode jobs; post-publication hooks register the output in `media_assets`/`media_files`, link `media_relationships` (`transcoded_from`), inherit manual tags, and preserve VR/color metadata.

### ADR-006: Shared Analysis Pipeline & Optional Local ML (OCR, Semantic Search, Faces, Local LLM)
- **Decision**: Share FFmpeg adaptive frame sampling across thumbnails, perceptual hashing, black/frozen-frame detection, OCR/watermark analysis, semantic embeddings, and opt-in face grouping.
- **Optional ML Workers**: Core app runs with zero mandatory ML downloads using pure-Rust visual/spatial descriptors, subtitle extraction, and static-watermark variance detection. When local ONNX/Python workers or models (PaddleOCR/RapidOCR for Chinese+English OCR, CLIP/SigLIP for semantic search, face embeddings) or local LLM endpoints (Ollama / LM Studio / OpenAI-compatible HTTP) are enabled, they integrate cleanly via typed worker interfaces, explicit model version tags, and preview-before-execute validation.

### ADR-007: Compact Multi-View Desktop UI
- **Decision**: Retain the lightweight vanilla HTML/CSS/JS frontend (`ui/`) using a shared media selection model across top-level views (`Library`, `Duplicates`, `Organize`, `People` when enabled, `Transcode`, `Repair`, `Tasks`, `Settings`), preserving all existing Queue/Preset/Profile/Preview/Cleanup workflows.
