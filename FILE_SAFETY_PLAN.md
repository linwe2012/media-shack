# Preventing unintended file loss in FFmpeg Queue

**Design handoff for Gemini Neon.** This plan covers original-file cleanup, duplicate removal, Sort to Folder, batch rename/move/copy, Undo, and recovery after interruption. It is based on a source review of the local working tree at commit `ce7a458` with uncommitted changes. Recheck the current handlers before editing and preserve work already in progress.

**Release decision.** Keep the affected destructive actions unavailable wherever their safety checks are incomplete. Users should still be able to scan, tag, preview plans, encode with Keep originals, and inspect outputs. Copy/export is suitable once its destination is protected against overwrite. A platform can ship with source cleanup unavailable. The UI should explain the specific reason an action is unavailable.

## 1. Make every Trash action honor the user's actual selection

The queue organizer shares one selection across tabs. Its backend accepts job IDs without confirming they belong to the displayed queue. It also fingerprints whatever file currently occupies a selected path. Meanwhile, unattended post-encode cleanup can bypass the Protected star.

- Give each queue its own selection. Before a destructive action, show the queue, exact filenames and parent folders, total count and size. Include only the items actually reviewed. Bind that review to their stable volume/file identities and fingerprints. If the selection, group, queue or file changes, require a fresh review. For unattended cleanup, one valid batch approval should cover exactly the approved jobs overnight.
- Put the final checks in one backend path used by queue Trash, Library duplicates, manual original cleanup and unattended cleanup. Carry the reason for removal so each workflow gets its own checks. Reject the whole request before touching any file if it contains an out-of-scope ID. Immediately before each OS operation, check protected status, identity, and references from all queues under the same coordination used for enqueue, retry and protection changes.
- Store protection by file identity and carry it forward through verified organizer moves. If a user marks a file Protected after giving batch consent, that later decision wins. A read error while loading protection or references must leave the file in place.
- For duplicates, let the user choose the keeper. For exact duplicates, verify a distinct, accessible, matching file survives outside the selection. Two jobs pointing to the same physical file count as one file. Keep visually similar items out of automatic bulk selection. Show each comparison for explicit review because a clip can share pictures while containing different audio or an ending worth keeping.
- Report outcomes per item. If item two changes after item one was moved, record the first move, leave the second file alone, and show the reason. A missing file should never increment the “moved to Trash” count.

**Where to start:** `ui/app.js` (`orgSelectedDeleteIds`, `trashOrgSelected`), `ui/platform.js` (Library duplicates), `commands.rs` (`trash_tab_specific_items`, `trash_tab_duplicates`, `trash_selected_originals`), `catalog_commands.rs` (`duplicates_trash_copies`), and `scheduler.rs` → `cleanup.rs` for unattended cleanup. Rust filenames here are under `src-tauri/src/`.

## 2. Require a verified output and a recoverable removal

- Post-encode cleanup requires a completed full-length encode, all required verification, successful publication, and a recorded identity for the verified output. Bind the verification to the staged file and carry that identity through publication. A successful encode can remain Completed even when its output fingerprint could not be recorded. In that case cleanup is ineligible, including for older database rows with missing evidence. Matching size alone is insufficient. Revalidate the output and its content before removing the source.
- On Windows the current shell delete requests undo on a best-effort basis. A fixed NTFS drive and a successful API return do not establish recoverability for a particular file. Require recoverable-only behavior for the actual operation. If the Recycle Bin is disabled, the file exceeds its limits, or a safe mechanism cannot be established, keep the source. [Microsoft shell operation flags](https://learn.microsoft.com/en-us/windows/win32/api/shellapi/ns-shellapi-shfileopstructw#members)
- On both platforms, resolve the gap between verifying an open file and sending a pathname to the OS. Require evidence that the operation acts on the verified item even if a file or ancestor is renamed concurrently. Checking the resulting Trash item can aid diagnosis after a move but cannot prevent moving the wrong item. Document the actual platform guarantee and leave cleanup disabled on any platform/filesystem where it cannot be met. Preserve the existing prohibition on permanent-delete fallback.
- Persist an item-specific cleanup intent and its outcome. On restart, surface ambiguous operations for review. Never repeat a removal merely because the old pathname exists again.

**Where to start:** `cleanup.rs` (`execute_cleanup_for_job`), `scheduler.rs` (publication fingerprint and cleanup dispatch), `platform/macos.rs` and `platform/windows.rs` (`move_to_recoverable_trash_verified`).

## 3. Give both organizers one preview and execution path

Today Sort to Folder flattens filenames and can fall back from any rename error to copying directly to the destination and unlinking the source. Batch Organize has separate preview and history logic, but a file that appears after preview can be overwritten by its final rename.

1. **Preview the complete batch.** Resolve all source identities and destinations together. Show old path → new path. Offer **Preserve subfolders** for tagged files or a flat destination with explicit collision handling. Flag both rows when two inputs resolve to the same name, including case-equivalent names on the destination filesystem. Default to keeping existing files. Offer a numbered filename, skip, or another destination. Invalidate stale previews when the user changes selection, operation, template or destination.
2. **Execute the reviewed plan.** Store the plan in the backend and pass a plan ID from the UI. Revalidate before mutation. Use atomic no-replace operations for rename, publication, rollback and Undo, including on filesystems with different case or naming rules. If a destination appears after preview, retain the source or its identified staging file and report the conflict. Apply this rule to Copy as well as Move.
3. **Treat a cross-volume move as a verified copy followed by a distinct source-removal step.** Only a confirmed cross-volume condition should trigger that workflow. Copy from the verified source into a private, exclusively created staging file. Flush, verify the full content, and publish without overwriting. Recheck the source identity and protected status before removing it. If safe source removal cannot be completed, report **Copied; original retained**. Record both paths. Other rename failures should surface their actual error.
4. **Keep the library and queues in sync.** Once a move commits, update the catalog, tags/protection association, and affected job references across all tabs. Revoke affected cleanup approvals and revalidate pending jobs against the new location. If a running job holds the source, defer the move. Keep completed history as an accurate account of the paths used at the time.

**Where to start:** `commands.rs` (`tab_move_by_tag`), `catalog_commands.rs` (`organize_execute_batch`), and `organize.rs` (`execute_batch_file_operations`, `copy_and_verify_blake3`). Reuse the transcode publisher's no-replace primitive where its platform guarantees apply.

## 4. Make Undo and restart preserve the remaining good copy

- Journal each item's original identity, source, staging path, intended destination and phase **before** its first filesystem change. Record each successful transition. For a batch rename cycle, record all intermediate names before staging the first file. Stop further mutation if recording an intent fails.
- For **Undo Copy**, verify that the original still exists as a distinct readable file with matching content and that the destination is the exact copy the app produced. If the original is gone or changed, retain the copy and explain why. Use recoverable disposal for an eligible copy. For **Undo Move**, verify the current destination identity and reserve the original path without overwriting anything that arrived later.
- On restart, reconcile recorded identities and phases. If either path now holds an unexpected file, preserve it and show both locations. Restrict temporary-file cleanup to individually recorded, positively identified app-owned items. The existing startup substring check for `.ffq-staging` is insufficient authority for recursive removal. Preserve unexpected contents for review.

**Where to start:** `organize.rs` (`undo_operation`, `operation_history` writes) and `scheduler.rs` (`reconcile_on_launch`).

## Hands-on release gates

Run these through the packaged UI on macOS and Windows using disposable copies, reference checksums and an isolated destination. Record the build, OS, filesystem, before/after paths and observed results. A planned or unrun check remains open.

| Try this | Required result |
| --- | --- |
| Select items in Family, switch to Travel and choose Trash. | Travel shows only its selection. Every Family file survives. |
| Approve overnight cleanup, then mark an approved source Protected before its job finishes. | The output completes. The original stays at its path and the history explains why. |
| Replace a selected source before confirming Trash. Separately replace a verified output with an equal-size file before original cleanup. | Both replacements survive. Cleanup is refused and the job explains what changed. Also check a legacy job missing its output fingerprint. |
| Select every member of an exact-duplicate group, remove the keeper externally, or queue the selected file for another job. Try a visually similar pair with different audio. | The app protects the last accessible exact copy and pending jobs. Visual similarity alone never authorizes removal. |
| Move a disposable source to Trash on a supported volume and restore it using the OS. Repeat on Windows with recycling disabled and with a file above the bin limit. | The successful restore matches the reference checksum. Unsupported or nonrecoverable operations leave the source at its original path. |
| Preview two clips named intro.mp4 from different folders. Then create a file at an approved destination before clicking Apply. | The preview explains the duplicate name. Execution preserves the later arrival and both original clips. |
| Move a tagged file across volumes while two queues reference it. Interrupt the copy or disconnect the destination before verification. | The source survives interruption. A successful move updates both queues and the catalog. An incomplete source removal is reported as a retained original. |
| Copy a file, take the original offline and click Undo. Also try Undo Move after putting another file in its old location. | The remaining copy and the newly arrived file both survive. The UI names the conflict. |
| Quit forcibly during batch staging and reopen. Place an unrelated file in a disposable staging directory before recovery. | The app accounts for every staged source, preserves the unrelated file, and reports any operation requiring review. |

**Completion criterion.** A tester can establish which physical files were approved, which were actually moved, where the surviving copy is, and how to recover from an interruption. Any unexpected removal, overwrite, or false success blocks the affected feature on that platform.
