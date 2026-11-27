# MediaShack — Unified Media Management and Archival Platform

## Implementation Specification v1.0

Repository: https://github.com/linwe2012/media-shack

## 1. Project objective

Extend the existing MediaShack application from a local batch video transcoder into a comprehensive, high-performance, privacy-first media management and archival platform.

The application should combine the capabilities of:

- HandBrake: video transcoding.
- Krokiet/Czkawka: duplicate and similar media detection.
- Adobe Bridge: metadata, tagging and organization.
- Immich: intelligent media search and organization.
- Untrunc: damaged video diagnosis and recovery.

The application must remain local-first, lightweight, cross-platform, and suitable for managing large media collections distributed across multiple hard drives.

The fundamental architectural principle is:

**Scan once, cache everything, and reuse analysis across features.**

Do not build independent tools that repeatedly scan and analyze the same files.

---

## 2. Existing architecture and constraints

MediaShack currently uses:

- Tauri 2 desktop application.
- Rust backend.
- Lightweight HTML/CSS/JavaScript frontend.
- SQLite through rusqlite.
- Tokio for asynchronous operations.
- FFmpeg and FFprobe for media processing.
- Persistent transcoding queues.
- Platform-specific file identity tracking.
- Source-safe cleanup and verification.

Relevant existing files include:

- `src-tauri/src/db.rs`
- `src-tauri/src/models.rs`
- `src-tauri/src/scanner.rs`
- `src-tauri/src/scheduler.rs`
- `src-tauri/src/runner.rs`
- `src-tauri/src/verify.rs`
- `src-tauri/src/preview.rs`
- `src-tauri/src/cleanup.rs`
- `src-tauri/src/platform/`
- `src-tauri/src/commands.rs`
- `ui/app.js`
- `ui/index.html`
- `ui/styles.css`

Inspect the current implementation before making changes. Reuse existing components where appropriate.

Do not rewrite the application or replace Tauri with Electron.

The existing transcoding functionality must continue working throughout development.

A separate recovery-engine implementation may already be in progress. Coordinate interfaces with that work rather than implementing a conflicting recovery subsystem.

---

## 3. Architectural principles

### 3.1 Rust-first implementation

Rust should be responsible for:

- Filesystem scanning.
- File identity tracking.
- Hashing and fingerprinting.
- Database operations.
- Metadata management.
- Media similarity algorithms.
- File operations.
- Scheduling and resource management.
- External process orchestration.
- Recovery and verification.
- Local ML inference where practical.

Use existing optimized Rust libraries rather than unnecessarily implementing low-level functionality from scratch.

Python is acceptable for specialized ML components when native Rust alternatives are impractical, but it must remain an optional worker rather than a mandatory dependency for the core application.

Prefer ONNX Runtime or other cross-platform native inference runtimes for production deployment.

### 3.2 Local-first and privacy-first

- No cloud account required.
- No mandatory internet connection after dependencies and models are installed.
- No uploading media or analysis results to external services.
- Optional AI functionality.
- All metadata and generated embeddings remain local by default.
- Users must be able to delete cached analysis and biometric data.

### 3.3 Non-destructive by default

- Index files in their existing locations.
- Do not automatically copy files into an application-managed library.
- Do not modify source media merely to assign tags.
- Never automatically delete similar videos.
- Always preview destructive or bulk file operations.
- Verify successful copies, repairs and transcodes before allowing source cleanup.

### 3.4 Cross-platform support

Initially target:

- Windows 11 x64.
- macOS Apple Silicon.
- macOS Intel where existing dependencies permit.

The application should run comfortably on a Mac with 16GB RAM.

---

## 4. Unified persistent media catalog

Introduce a persistent media catalog independent of the current encoding queues.

The catalog is the foundation for every subsequent feature.

### 4.1 Media identity model

Distinguish between:

1. Logical media asset.
2. Physical file instance.
3. Current filesystem location.
4. Derived or alternate media versions.

A logical asset may have multiple versions, including:

- Original video.
- Transcoded video.
- Remuxed video.
- Recovered video.
- Lower-resolution copy.
- Partial clip.

A physical file may have multiple filesystem paths, such as hard links.

Never use a filename or path as the primary media identity.

Use:

- Persistent UUID.
- Platform file identity.
- Volume identity.
- File size.
- Modification timestamp.
- Partial content fingerprint.
- Full content hash when necessary.

Handle external renames and moves without losing tags or analysis results.

When a file changes, distinguish between metadata-only changes, content changes, and replacement by an entirely different file.

Do not automatically merge logical assets merely because perceptual similarity is high.

### 4.2 Database schema

Extend the existing SQLite database through versioned migrations.

Proposed logical tables:

| Table | Responsibility |
|---|---|
| `library_roots` | Registered directories and drives |
| `volumes` | Storage volume identities and availability |
| `media_assets` | Logical media records |
| `media_files` | Physical file instances and technical metadata |
| `file_locations` | Current and historical paths |
| `media_relationships` | Original, derivative, recovered and partial relationships |
| `tags` | Tag definitions |
| `media_tags` | Many-to-many tag assignments |
| `collections` | Saved searches and virtual collections |
| `fingerprints` | Exact and perceptual fingerprints |
| `similarity_edges` | Verified or candidate similarity relationships |
| `analysis_results` | OCR, classification and other analysis |
| `media_embeddings` | Semantic search vectors |
| `analysis_jobs` | Persistent analysis tasks |
| `operation_history` | Rename, move, copy and undo operations |
| `integrity_results` | Checksum and decoding verification |
| `metadata_sync_state` | External metadata synchronization status |

The precise schema may be adjusted after inspecting existing database structures.

Requirements:

- Foreign-key constraints.
- Appropriate indexes.
- Transactional migrations.
- Efficient batch inserts.
- SQLite WAL mode.
- Full-text search using FTS5 where supported.
- Explicit algorithm and model versions for cached analysis.
- Database backup and export support.
- No large video blobs inside SQLite.

Store thumbnails, extracted frames and other large artifacts in a separate managed cache directory.

### 4.3 Library scanning

Allow users to register:

- Individual files.
- Multiple directories.
- Entire drives.
- External hard drives.
- Network-mounted directories.

Support:

- Recursive scanning.
- Incremental updates.
- Cancellation and pause.
- Exclusion rules.
- Hidden-file handling.
- Symlink and junction safety.
- Unicode and Chinese filenames.
- Long Windows paths.
- Disconnected and reconnected drives.

Disconnected media must remain in the database and be marked offline rather than deleted.

Use filesystem notifications where practical, supplemented by periodic reconciliation scans.

Do not rely exclusively on filesystem notifications because events may be missed.

### 4.4 Lazy analysis

Do not perform expensive analysis on every imported file immediately.

Define analysis levels:

- Level 0: filesystem metadata.
- Level 1: FFprobe technical metadata.
- Level 2: thumbnails and lightweight fingerprints.
- Level 3: full hashes and perceptual fingerprints.
- Level 4: OCR and semantic analysis.
- Level 5: deeper content analysis and integrity verification.

The user should be able to choose which levels to execute.

Cache results and invalidate them only when the underlying content or algorithm version changes.

---

## 5. Duplicate and similar media detection

Implement multiple detection methods rather than a single similarity score.

### 5.1 Exact duplicate detection

Pipeline:

1. Group files by file size.
2. Exclude groups containing only one file.
3. Calculate partial fingerprints.
4. Calculate full hashes for remaining candidates.
5. Verify exact duplicate groups.

Prefer BLAKE3 for fast content hashing. Also support SHA-256 for archival integrity and interoperability.

Requirements:

- Buffered or memory-mapped reading where appropriate.
- Bounded parallelism.
- Storage-aware concurrency.
- Persistent hash caching.
- Progress measured in bytes processed.
- Pause and cancellation.
- Avoid redundant hashing of hard-linked files.

### 5.2 Equivalent encoded media

Detect media containing the same encoded streams but packaged differently.

Examples:

- MP4 remuxed into MKV.
- Identical video with different container metadata.
- Identical video with different audio tracks.

Use normalized encoded-stream fingerprints where supported.

Account for container-specific representations, codec configuration data and packetization differences.

Distinguish:

- Identical complete media streams.
- Identical video but different audio.
- Identical video with different subtitles.
- Identical video with different container metadata.

Do not classify these files as byte-identical duplicates.

### 5.3 Perceptually similar videos

Detect videos that contain the same visual content despite:

- Different codecs.
- Different bitrates.
- Different resolutions.
- Minor color adjustments.
- Watermarks.
- Small borders.
- Moderate compression artifacts.

Suggested pipeline:

1. Extract representative frames using FFmpeg.
2. Normalize frame dimensions and color representation.
3. Compute perceptual hashes.
4. Store timestamped fingerprints.
5. Retrieve likely candidates using an efficient similarity index.
6. Compare temporal fingerprint sequences.
7. Verify similarity using additional sampled frames.

Evaluate pHash, dHash, DCT-based fingerprints and other established methods.

Reuse relevant open-source implementations where licenses permit.

Reference projects:

- https://github.com/qarmin/czkawka
- https://github.com/slowriot/videohash
- https://github.com/facebook/ThreatExchange

Do not perform an unrestricted O(N²) comparison across the entire catalog.

Use candidate indexing, duration ranges, locality-sensitive hashing, multi-index hashing or other scalable retrieval techniques.

Duration should be a soft candidate signal rather than an absolute requirement because some matching videos may be trimmed.

### 5.4 Partial and temporal overlap detection

Support finding:

- Short clips extracted from longer videos.
- Videos with different introductions or endings.
- Videos containing additional advertisements.
- Different edits of the same recording.
- Partially recovered videos.

Use timestamped fingerprint sequences and temporal alignment.

Consider offset voting, sequence matching and dynamic time warping where appropriate.

Report:

- Matching interval in each video.
- Estimated overlap duration.
- Percentage of each video covered by the overlap.
- Confidence.
- Whether the match is full-length or partial.

### 5.5 Similarity results and quality comparison

Display candidate groups with:

- Preview thumbnails.
- File paths.
- File sizes.
- Duration.
- Resolution.
- Codec.
- Bitrate.
- Audio and subtitle tracks.
- Integrity status.
- Similarity confidence.
- Storage savings from removing selected redundant copies.

Provide suggestions for which copy to retain, but never automatically assume that:

- The largest file is best.
- The highest-resolution file is best.
- HEVC is always superior to H.264.
- A newer file is necessarily better.

If an original and its transcoded derivative are known, preserve that relationship explicitly.

Allow synchronized playback and side-by-side comparison.

For near-duplicates, require manual review before cleanup.

Support merging compatible tags from a redundant copy into the retained asset.

---

## 6. Tagging and media organization

### 6.1 Tag system

Implement database-backed tags as the primary tagging mechanism.

Features:

- Manual tag creation.
- Multiple tags per asset.
- Hierarchical tags or tag namespaces.
- Tag autocomplete.
- Tag aliases.
- Tag merging.
- Bulk assignment and removal.
- Unicode support.
- Optional colors.
- Tag usage counts.
- Separate manual, imported and AI-suggested tags.

Example namespaces:

- `creator:Example`
- `location:Colorado`
- `format:VR180`
- `activity:Skiing`
- `status:Archived`
- `project:Vacation`

AI-generated tags must retain their provenance and confidence.

Manual user corrections must take precedence over later automated analysis.

### 6.2 Filtering and search

Support combinations of:

- Filename.
- Directory.
- Tags.
- Excluded tags.
- Resolution.
- Codec.
- Bitrate.
- Duration.
- File size.
- Recording date.
- Modification date.
- Integrity status.
- Duplicate group.
- Creator.
- OCR text.
- Media type.
- Online/offline availability.

Support AND, OR and NOT operations.

Allow saving filter combinations as virtual collections.

### 6.3 Virtual collections

Virtual collections are saved queries, not physical directories.

A file may appear in several collections without being copied.

Examples:

- All VR180 videos.
- All videos larger than 5GB.
- All H.264 videos awaiting archival.
- All videos with detected watermarks.
- All media without tags.
- All suspected duplicates.
- All corrupted videos.
- All files belonging to a particular creator.

Selections from virtual collections must support all applicable batch operations.

### 6.4 Batch rename

Implement a template-based rename engine.

Example template:

`{creator}_{date}_{title}_{resolution}_{index}.{extension}`

Supported variables should include:

- Original filename.
- Title.
- Creator.
- Date and year.
- Parent directory.
- Resolution.
- Codec.
- Duration.
- Index.
- Custom tags.
- User-defined text.

Support:

- Find and replace.
- Regular expressions.
- Case conversion.
- Prefix and suffix.
- Sequential numbering.
- Removal of unwanted text.
- Unicode normalization where explicitly selected.

Always provide a before-and-after preview.

Validate Windows reserved names, invalid characters, maximum path lengths and filename collisions.

Handle rename cycles safely using temporary intermediate filenames.

### 6.5 Batch move and copy

Allow users to select files by tags or collections and:

- Move them into one directory.
- Distribute them into generated directory structures.
- Copy them to another drive.
- Export a selected collection.
- Rename and move in one operation.

Example folder template:

`Media/{creator}/{year}/{title}.{extension}`

Define explicit behavior when a file has several values for a template field.

Never silently duplicate files into multiple folders merely because they have multiple tags.

For cross-volume moves:

1. Copy to a temporary destination.
2. Verify the copy.
3. Commit the destination.
4. Remove the original only after successful verification and authorized move execution.

All operations must have durable journal entries.

Support undo where the original destination remains available and no conflicting changes have occurred.

---

## 7. Windows and macOS metadata interoperability

SQLite remains the primary catalog.

Operating-system and embedded metadata integration should be optional.

### 7.1 Windows

Implement Windows Property System integration:

- `System.Keywords`
- `PKEY_Keywords`
- `IPropertyStore`

For supported formats, allow MediaShack tags to appear in Windows Explorer's Details panel and Tags column.

Detect whether the installed property handler supports writing tags for a particular format.

Do not assume that arbitrary file formats support native Windows tags.

### 7.2 macOS

Support optional synchronization with Finder tags using supported macOS APIs and filesystem metadata.

Detect filesystem limitations and report unsuccessful synchronization.

### 7.3 Embedded metadata

Support reading and optionally writing appropriate metadata for:

- MP4.
- MOV.
- MKV.
- JPEG.
- PNG where applicable.
- Other supported media formats.

Evaluate ExifTool as an external metadata tool:

https://exiftool.org/

Metadata writing must not re-encode the video.

Preserve unrelated metadata, color information, audio tracks, subtitles and VR projection information.

Because metadata modifications change file hashes, update integrity records and invalidate affected fingerprints appropriately.

### 7.4 Portable sidecar metadata

Support JSON and optionally XMP sidecars.

Allow export and import of:

- Tags.
- Descriptions.
- Creator information.
- Recording dates.
- Collections.
- Media identifiers.
- Content fingerprints.

This provides a portable backup if the user moves media to another computer or reinstalls MediaShack.

Implement explicit conflict resolution between database metadata, embedded metadata, sidecars and operating-system tags.

Do not silently overwrite conflicting user edits.

---

## 8. Shared media analysis pipeline

Create a reusable media analysis subsystem.

FFmpeg frame extraction should be shared between:

- Thumbnail generation.
- Perceptual duplicate detection.
- Watermark detection.
- Scene classification.
- Semantic search.
- Optional face analysis.
- Black-frame and frozen-frame detection.

Avoid decoding the same video repeatedly when one extraction operation can serve multiple requested analyses.

### 8.1 Adaptive frame sampling

Support:

- Fixed time intervals.
- Percentage-based sampling.
- Scene-change detection.
- Keyframe-based sampling.
- Higher-density sampling for uncertain results.

Use inexpensive sampling initially, then perform deeper analysis when needed.

Store timestamps with every frame-derived result.

For VR180/VR360 videos, account for stereoscopic layouts and projection distortion.

Where necessary, analyze one eye or projected perspective views to avoid duplicated detections.

Never modify the original stereoscopic layout as part of analysis.

### 8.2 Cache management

Cache artifacts according to:

- Media content identity.
- Analysis type.
- Algorithm version.
- Model version.
- Sampling configuration.

Support configurable cache size limits and eviction policies.

Do not invalidate expensive visual analysis merely because the user renamed a file or edited its catalog tags.

---

## 9. OCR and automatic watermark detection

Implement lightweight local OCR for Chinese and English.

Evaluate PaddleOCR and suitable ONNX-exported OCR models.

Reference:

https://github.com/PaddlePaddle/PaddleOCR

The OCR pipeline should:

1. Sample representative video frames.
2. Identify text regions.
3. Recognize text.
4. Record bounding boxes and timestamps.
5. Identify text that persists across multiple frames.
6. Distinguish likely watermarks from changing subtitles.
7. Suggest creator names, titles and tags.

Support detection of:

- Creator usernames.
- Website watermarks.
- Titles.
- Dates.
- Chinese and English text.
- Persistent logos containing text.
- Subtitles.

Use temporal and spatial consistency to rank watermark candidates.

Store:

- Raw recognized text.
- Normalized text.
- Confidence.
- Timestamp.
- Bounding box.
- Frequency across sampled frames.
- Suggested classification.
- User confirmation status.

Provide configurable text normalization and creator aliases.

For example, multiple variations of the same username may be associated with one creator tag after review.

Do not invent creator names when no reliable evidence exists.

---

## 10. Optional face grouping

Implement an opt-in, entirely local feature for organizing personal media into anonymous face-similarity groups.

The intended functionality is similar to the People view in personal photo-management applications.

Requirements:

- Process only user-selected media libraries.
- Detect visible faces in sampled frames.
- Group visually similar face observations locally.
- Allow users to review suggested groups.
- Allow manual labels.
- Allow merging and splitting incorrectly grouped observations.
- Allow excluding specific faces or media.
- Support multiple groups associated with one video.
- Show timestamps at which relevant observations occur.

Do not attempt to determine a person's real-world identity automatically, search external face databases or identify strangers through internet searches.

Do not infer demographic attributes, health information or other sensitive characteristics from faces.

Provide conservative matching behavior and an unassigned group for uncertain observations.

Protect locally stored biometric information and allow permanent deletion of face crops, embeddings and associated analysis.

Make this subsystem independent and optional.

Evaluate existing local inference components and their licensing before implementation. Do not introduce a large mandatory ML dependency solely for this feature.

---

## 11. Semantic media search

Implement natural-language search across media content.

Example queries:

- People skiing in snowy mountains.
- Videos filmed near the ocean.
- Programming screen recordings.
- Videos containing cats.
- Videos with Chinese subtitles.
- Recordings made outdoors at night.

Evaluate lightweight image-text embedding models such as CLIP, OpenCLIP or SigLIP.

Pipeline:

1. Generate visual embeddings from sampled frames.
2. Store embeddings in the local catalog.
3. Generate a text embedding from the search query.
4. Retrieve candidate matching frames and media.
5. Rank by visual similarity and optional metadata filters.
6. Display the matching timestamps.

Support both:

- Media-level search.
- Timestamp-level search within videos.

Evaluate SQLite vector-search extensions or a persistent local approximate-nearest-neighbor index.

Store embedding model versions and dimensions explicitly.

Do not combine embeddings generated by incompatible model versions.

Start with efficient exact vector search for smaller libraries before introducing a more complicated indexing dependency.

A large language model should not be required to process every video frame.

---

## 12. Optional local language model integration

Support optional integration with:

- Ollama.
- LM Studio.
- OpenAI-compatible local endpoints.

Potential use cases:

- Normalize OCR results.
- Generate suggested tags.
- Summarize detected content.
- Generate descriptive filenames.
- Convert natural-language organization instructions into structured rules.
- Resolve inconsistent metadata.

Examples:

- Group videos from the same creator.
- Rename all selected videos using detected titles.
- Find high-resolution videos that have not been archived.
- Suggest a directory structure for unorganized media.

The model should generate proposed structured operations, not directly execute unrestricted filesystem commands.

Validate all model-generated operations through the existing file-operation engine and present previews before execution.

Keep this integration optional and avoid making the application dependent on a continuously running LLM.

---

## 13. Media integrity and recovery integration

Integrate the separate video-recovery implementation into the shared catalog.

The catalog should track:

- FFprobe readability.
- Container integrity.
- Missing MP4 atoms.
- Truncated media data.
- Decoder errors.
- Audio/video stream validity.
- Verification results.
- Recovery attempts.
- Recovered media relationships.

Provide:

- Lightweight structural integrity checks.
- Full decoding verification.
- Batch corruption detection.
- Repair task queues.
- Recovery candidate comparison.
- Recovery logs.
- Partial recovery support.

Prioritize lossless repair and original encoded-stream recovery.

Never replace the damaged source automatically.

A successful recovery must be established through actual decoding and output verification, not merely the existence of an output file.

Preserve tags and logical relationships between damaged originals and recovered versions.

Refer to the separate MP4 recovery specification for implementation details.

---

## 14. Transcoding integration

Preserve the existing encoding engine.

Allow any library selection or virtual collection to be sent directly to a transcoding queue.

Examples:

- All H.264 videos larger than 2GB.
- All VR videos not already encoded in HEVC.
- All verified videos awaiting archival.
- All videos within a selected duplicate group.

After transcoding:

- Register the resulting file.
- Establish its relationship to the original.
- Inherit appropriate tags.
- Preserve relevant metadata.
- Verify output integrity.
- Update storage statistics.
- Record encoding parameters and quality settings.

Do not blindly copy technical metadata or perceptual analysis that may no longer be valid for the derivative.

Support existing CPU and GPU encoding workflows.

Preserve stereoscopic layouts, projection metadata and headset compatibility for VR videos.

Allow the user to compare original and transcoded versions before source cleanup.

---

## 15. Archival integrity management

Implement persistent integrity records.

Support:

- SHA-256 checksum manifests.
- BLAKE3 checksums for fast local verification.
- Scheduled or manually triggered verification jobs.
- Missing-file detection.
- Changed-file detection.
- Storage-volume health warnings where available.
- Backup availability tracking.
- Checksum export.

Clearly distinguish between:

- An intentional modification.
- An unexpected checksum change.
- An inaccessible storage volume.
- A missing file.
- A decoding failure.

Checksums detect corruption but cannot repair it.

Provide optional integration with recovery-parity tools such as PAR2 in a later phase.

Do not claim that a single archived copy is protected against disk failure.

---

## 16. Task scheduling and resource management

Extend the existing scheduler into a shared resource-aware task manager.

Task categories:

- Filesystem scanning.
- Metadata extraction.
- Thumbnail generation.
- Hashing.
- Similarity analysis.
- OCR.
- Semantic indexing.
- Optional face analysis.
- Integrity verification.
- Video repair.
- Transcoding.
- File operations.

Each task should have:

- Persistent identifier.
- Input media references.
- Task type.
- Priority.
- Progress.
- Current state.
- Error information.
- Retry policy.
- Resource requirements.
- Cancellation support.
- Resume or restart behavior.

Support configurable resource budgets:

- CPU worker count.
- GPU concurrency.
- Memory limits.
- Disk I/O concurrency.
- Maximum simultaneous FFmpeg processes.
- Cache size.

Account for the difference between mechanical HDDs and SSDs.

Avoid running multiple intensive random-access operations on the same HDD unnecessarily.

Allow analysis to pause or throttle while transcoding.

Support overnight operation and existing keep-awake functionality.

Do not block the frontend while performing long-running operations.

Use bounded queues and backpressure instead of spawning unlimited tasks.

---

## 17. Frontend design

Retain the current lightweight frontend architecture.

The interface should resemble a professional desktop media manager rather than a dashboard-heavy website.

Vertical space is valuable.

Requirements:

- Compact toolbars.
- Minimal padding.
- Resizable panels.
- Dense table views.
- Optional thumbnail grid.
- Virtualized or paginated large lists.
- Keyboard shortcuts.
- Multiselection.
- Context menus.
- Persistent filters.
- Fast tag editing.
- Configurable columns.
- Sortable tables.
- Split-view comparison.
- Dark and light modes.

Suggested primary navigation:

- Library.
- Duplicates.
- Organize.
- People, if enabled.
- Transcode.
- Repair.
- Tasks.
- Settings.

Use a shared media-selection model so users can perform operations without repeatedly importing files into different modules.

### Library layout

- Left sidebar: drives, directories, collections and tags.
- Top toolbar: search, filters and bulk operations.
- Main area: media table or thumbnail grid.
- Optional right panel: metadata, tags, preview and analysis.
- Compact bottom status bar: selection, total size and active task progress.

Avoid unnecessary nested dialogs.

### Duplicate review layout

- Group list.
- Selected group contents.
- Side-by-side previews.
- Technical metadata comparison.
- Keep/remove recommendations.
- Estimated storage savings.
- Explicit cleanup controls.

### Task manager

Show all background operations in one place.

Provide pause, resume, cancel and priority controls.

Display actual throughput where useful, such as MB/s, frames/s or files/s.

---

## 18. Testing requirements

### Database and identity tests

- Schema migration from existing installations.
- Database backup and restore.
- Interrupted transactions.
- Same-volume renames.
- Cross-volume moves.
- Hard links.
- External file modifications.
- Disconnected drives.
- Unicode filenames.
- Multiple paths referring to the same file.

### Duplicate detection tests

Generate known test media and variants:

- Byte-identical copies.
- Remuxed copies.
- Different resolutions.
- Different codecs.
- Different bitrates.
- Watermarked copies.
- Trimmed versions.
- Cropped videos.
- Videos with different introductions.
- Unrelated videos with similar scenes.

Measure precision, recall and false-positive rates.

### File-operation tests

- Filename collisions.
- Rename cycles.
- Invalid Windows filenames.
- Case-only renames.
- Insufficient storage.
- Permission errors.
- Interrupted copies.
- Cross-volume verification failures.
- Undo conflicts.

### Metadata tests

- Windows tag round trips.
- Finder tag round trips.
- Embedded metadata preservation.
- Sidecar import/export.
- Conflicting metadata.
- Preservation following transcoding.
- Preservation of VR metadata.

### ML and search tests

- Chinese and English OCR.
- Persistent watermark detection.
- Subtitle versus watermark distinction.
- Uncertain classification handling.
- Model-version invalidation.
- Semantic search relevance.
- Optional face-group review and deletion.

### Performance tests

Test libraries containing:

- 1,000 media files.
- 10,000 media files.
- 100,000 media files.

Measure:

- Initial scan time.
- Incremental scan time.
- Memory usage.
- Database size.
- Cache size.
- Search latency.
- Duplicate-analysis throughput.
- UI responsiveness.

Use generated fixtures and measured results rather than unsupported performance claims.

---

## 19. Implementation roadmap

### Milestone 1 — Persistent media catalog

Implement:

- Database migrations.
- Library roots.
- Media identities and locations.
- Incremental scanning.
- FFprobe metadata caching.
- Library table.
- Search and filtering.
- Offline-drive handling.

Acceptance criteria:

- Existing transcoding functionality still works.
- Media can be indexed without copying.
- Repeated scans reuse cached data.
- Renaming or moving files does not lose their catalog metadata.
- Large lists remain responsive.

### Milestone 2 — Organization

Implement:

- Manual tags.
- Bulk tagging.
- Virtual collections.
- Batch rename.
- Batch move and copy.
- Operation preview.
- Durable history and undo.

Acceptance criteria:

- Users can filter by multiple tags and organize matching media into directories.
- Interrupted operations do not destroy original files.
- Filename collisions are handled safely.

### Milestone 3 — Duplicate and similarity detection

Implement:

- Exact duplicate hashing.
- Encoded-stream equivalence.
- Perceptual video fingerprints.
- Temporal overlap detection.
- Duplicate review interface.
- Storage-savings calculations.

Acceptance criteria:

- Known duplicate copies are detected reliably.
- Transcoded variants are recognized as similar rather than exact duplicates.
- Partial clips can be matched to longer videos.
- Users can review before cleanup.

### Milestone 4 — Metadata interoperability

Implement:

- Windows native tags.
- macOS Finder tags.
- Embedded metadata.
- JSON/XMP sidecars.
- Metadata conflict resolution.

Acceptance criteria:

- Supported tags can round-trip between MediaShack and external tools.
- Unsupported file formats remain fully taggable through the SQLite catalog.
- Video streams are not re-encoded by tag synchronization.

### Milestone 5 — Shared task and archival integration

Implement:

- Shared task scheduling.
- Resource management.
- Integrity verification.
- Recovery integration.
- Transcoding relationships.
- Checksum manifests.

Acceptance criteria:

- Analysis and transcoding coexist without exhausting system resources.
- Recovered and transcoded files retain appropriate catalog relationships.
- Integrity status is visible from the library.

### Milestone 6 — OCR and intelligent tagging

Implement:

- Shared frame extraction.
- Local OCR.
- Watermark recognition.
- Suggested tags.
- Creator normalization.
- Review interface.

Acceptance criteria:

- Chinese and English watermarks can be extracted from representative test videos.
- Users can approve suggested creator tags in batches.
- Missing or uncertain information is not fabricated.

### Milestone 7 — Semantic search and optional grouping

Implement:

- Local image-text embeddings.
- Natural-language search.
- Timestamp-level results.
- Optional private face grouping.
- Optional local LLM integration.

Acceptance criteria:

- Users can search by visual content rather than filenames alone.
- Optional intelligence features work locally.
- Core application functionality remains available without ML dependencies.

---

## 20. Execution instructions for the coding agent

1. Inspect the current repository and existing work in progress.
2. Review `IMPLEMENTATION.md`, `TEST_PLAN.md` and existing integration tests.
3. Identify reusable database, scanner, scheduler and file-identity components.
4. Produce a concise architecture decision record.
5. Update the implementation documentation to reflect this specification without removing existing encoder requirements.
6. Break development into independently testable milestones.
7. Begin implementation with Milestone 1 unless the user explicitly requests planning only.
8. Run existing regression tests after every substantial change.
9. Keep the application functional between milestones.
10. Document limitations and incomplete functionality.
