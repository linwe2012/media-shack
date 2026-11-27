# FFmpeg Queue — hands-on acceptance test plan

Use this guide when a packaged build is available. Install it, open it from Finder or the Windows Start menu, drop real files into it and play the resulting videos in a separate player. Record a pass only for behavior witnessed on the named build and platform. All scenarios are currently unrun.

## Start here

For the first usable build, run **First launch to first movie**. For a release candidate, run the remaining scenarios on Windows 11 x64 and macOS, tracking Apple Silicon and Intel separately wherever support is claimed. Use the normal desktop account and record the FFmpeg build detected by the app. A scenario that needs a second display, a removable volume or a particular encoder stays **Untested** until exercised there.

Create an empty test destination and a source folder containing disposable copies. Keep reference copies outside every folder you will import. For cleanup testing, use a fresh test account or test volume where practical. Never empty the OS Trash or Recycle Bin to run this plan.

| Bring this test material | What it lets you observe |
| --- | --- |
| A short SDR video with audible speech or a clap, visible motion and a recognizable ending | First encode, audio sync, final frames and size comparison. Watch the original once. |
| A longer SDR clip with changing scenes and a visible clock or frame counter | Reading current frames during encoding. Choose enough footage and a slow enough preset to give yourself several minutes to operate the UI. |
| Two subfolders containing different videos with the same filename, plus a name with spaces and non-ASCII characters | Recursive import, queue membership and safe naming. |
| A multitrack MKV with two identifiable audio languages, subtitles, a chapter and a font attachment for styled subtitles | Track inspection, MP4 compatibility decisions and MKV preservation. |
| A rotated phone clip, a variable-frame-rate clip, a silent clip and an HDR clip with known metadata | Orientation, timing, silence and handling sources outside a preset's supported range. |
| A damaged copy that probes as video but fails during encoding, plus a file that cannot be probed | Distinguishing a staging problem from a mid-queue failure. If the damaged copy encodes cleanly, replace the fixture and repeat. |
| An isolated, size-limited writable test volume and a separate healthy destination | Safe simulation of a filling drive without filling the system disk. |

For the first walkthrough, only the short SDR clip and an empty destination are needed. For every playback check, use a player known to support the selected codec and container. Inspect the file's **Size** in Finder or Explorer when checking byte counts. “Size on disk” measures allocated space and can differ.

## First launch to first movie

| Step | Operate the app | Pass when you see this |
| --- | --- | --- |
| 1 | Install the packaged build and open its icon. On a clean machine, try once with the media tools unavailable. Use **Locate FFmpeg tools** to select an installed pair, then relaunch. | The app explains how to fix the missing dependency. The chosen tools survive relaunch. A usable first queue opens without creating a profile. |
| 2 | Drop the short clip into the empty queue. Pick the test destination. Expand the row before starting. | The row shows the right filename and byte size. Duration, dimensions, video codec and audio agree with the original's player information. The proposed output path and **Keep originals** are visible. |
| 3 | Leave **Archive → Balanced**, HEVC and MKV selected. Click **Start 1 ready file** or its equivalent. | The job advances from ready through encoding to completion. The window stays responsive. A short file may finish before an ETA appears. |
| 4 | Click **Show output**. Open the MKV independently, seek near the middle and play through the ending. Reopen the original. | Both videos play. Orientation, duration, audio sync and ending match your reference observations. The output reports HEVC in an MKV container. |
| 5 | Compare source and output sizes in the file manager with the job history. Quit and relaunch. | The ratio equals input bytes divided by output bytes within display rounding. The percentage and “smaller” or “larger” label agree. The completed row and its final values survive relaunch. |

**First-use observation.** Give a fresh tester the task “Make a smaller copy and keep the original.” Let them work without coaching. Record the first control they hesitate over and ask where they expect to find the output before they click Start. A technically successful encode can still reveal a confusing workflow.

## Independent queues and shared profiles

1. Save a profile called **Overnight**. Create **Family** and **Travel** queue tabs and select Overnight in both. Drop a nested folder on Family, switch to Travel while it scans, and add a different clip there. Toggle recursion on a separate import. Check both staging lists and output paths.
2. Start both queues with multiple files. While Family is encoding, inspect Travel's waiting reason. Select **Pause after current file** in Family. Reorder two waiting Travel jobs. Verify only one encode runs, the next eligible tab advances and each queue respects its own order.
3. Change the selected profile in Travel. Apply new settings to future additions first, then explicitly apply them to one or more waiting jobs. Inspect the planned codecs in both tabs. An in-flight encode and Family's existing jobs retain their settings.
4. Quit with pending work, relaunch and inspect the queues. Rename a tab and delete a disposable profile that both queues previously used. **Pass** if membership, order, history and saved job settings survive, and the two tabs remain independently operable.

## Preview and quality checks

**Before encoding.** Select the clock clip and choose **Preview 15 seconds**. Pick a recognizable scene away from the opening. Compare Original and Encoded at the same timestamp, play to the end, and zoom to 100%. Check the sample length, size and reported encode time. Change CRF and look for the stale-sample indication. Try a clip under 15 seconds. Pass if the sample uses the selected settings, ends at the source boundary and leaves the job queued and its original intact.

**During encoding.** On each platform, run the following matrix. Use source material long enough to let the encoder move well past its first frame. Record at least two displayed timestamps and a recognizable scene for each run.

| Container | HEVC Balanced | HEVC Best | AV1 Balanced | AV1 Best |
| --- | --- | --- | --- | --- |
| MKV | ☐ | ☐ | ☐ | ☐ |
| Standard MP4 | ☐ | ☐ | ☐ | ☐ |

- Start with the preview closed. Open it mid-job. Watch the clock in the image advance, close the panel, wait for progress to move, then reopen. A still opening frame, a stale image from another job or a restart of the encode is a failure.
- Save a screenshot with its timestamp. After the job completes, seek to that time in the final video and compare scene content. Inspect the file in a player to confirm the requested codec and container. Check the job's resolved settings in **Details** for effort and CRF. Ordinary MP4 should remain usable with its selected output settings.
- On one HEVC run and one AV1 run per OS, use Task Manager or Activity Monitor to end **only the identified preview worker** while pictures are updating. The archive must continue from its current position, complete once and pass playback and verification. If the worker cannot be identified reliably, mark the fault test **Blocked** and record what visibility is missing.
- On the same machine, repeat a representative encode with and without the monitor under comparable conditions. Record elapsed time and observed CPU/memory. Note blank time on opening, stale-frame duration and whether closing the viewer makes the UI more responsive.

## Files, exceptions and recovery

| Scenario | What to do | Expected result |
| --- | --- | --- |
| Mixed batch | Queue a valid clip, a mid-encode failure fixture and another valid clip. Also stage the unreadable fixture. Run once with **Continue**, then with **Pause on first error**. | The staging problem has a reason. Continue finishes both valid jobs. Pause holds the later job until resumed. The failed source survives and its partial stays out of the final output list. Retry targets the selected failed job. |
| Track compatibility | Select MP4 for the styled-subtitle MKV. Inspect the conflict and choose **Use MKV for this file**. Play the result, switch audio languages, enable subtitles and visit the chapter. | The app explains affected tracks before encoding. The MKV preserves the planned tracks, chapter, flags and attachment. Another queued MP4 job keeps its settings. |
| Phone and silent footage | Encode supported rotated, VFR and silent clips. Check orientation, timing at the beginning and end, and track count in an independent player. Stage the HDR fixture. | Supported footage preserves its presentation. The HDR file either follows an explicitly supported preservation path with verified metadata or goes to **Needs attention** with a reason. |
| Name collisions | Pre-create a recognizable file at the proposed output name. Import both same-name videos from different folders. Repeat with the Unicode filename and a duplicate drop. | Staging distinguishes the sources, exposes duplicate handling and reserves distinct final paths. The pre-existing file still has its original contents. |
| Cancel and quit | Cancel one active encode. On another, close the window, choose **Keep running**, reopen, then exercise **Stop and quit** with work pending. | Only the selected attempt cancels. Background work continues when requested. Relaunch restores pending work and explains the stopped attempt. |
| App crash | Force-quit the app during a long encode. Relaunch, inspect interrupted state and retry. Repeat near finalization on disposable files. | The encoder exits, or the supervisor reaps it. Recovery shows what completed or needs review. No duplicate or truncated final output appears. Record the phase actually hit; an ordinary force-quit does not prove every publication crash window. |
| Low space | Set the test volume's reserve above its current free space and start a job. Then set it just below current free space and run a large enough job to cross it. Queue a job for the healthy volume too. | Admission and mid-run cases both show the affected drive. Its jobs wait, the healthy destination can proceed, and originals remain. Free space or change destination, then retry. |
| Savings threshold | Encode once to measure the actual reduction. Using the same source and recipe, set a minimum above that observed reduction, then a minimum below it. Choose a source with a reduction between the two selectable thresholds. | The first attempt reports **Below savings target**, keeps the original and excludes the staged output from published totals. The qualifying attempt publishes normally. |
| Drive goes away | With disposable files on a detachable test volume, take the destination offline using a safe test setup, then reconnect and retry. | The app identifies the unavailable volume, preserves the source and waits for the correct destination. It never writes into a different folder that happens to reuse the path. |

## Source cleanup rehearsal

Use uniquely named disposable source copies. Before each case, record source and reference size and a checksum with a trusted file utility. Inspect the source folder, published output and OS Trash after each action. A surprising move, permanent removal, or cleanup of a failed encode blocks release of cleanup on that platform and volume type.

| Case | Action | Observable pass condition |
| --- | --- | --- |
| Default and declined consent | Finish a normal encode. In a separate batch select a profile that remembers automatic trashing, then decline its batch confirmation. | Both source copies remain at their original paths. |
| Confirmed recovery | On a supported local volume, approve one named source, run to completion and open the output. Find the original in OS Trash and restore it through the OS. | The encode and full verification completed before the move. The restored file matches the reference checksum and plays. If the app declares the volume unsupported, record the supported-path test as untested there. |
| Scope across queues | Queue one source in two tabs. Authorize cleanup for the first job and leave the other pending. Add a different file after consent. | The shared source remains available to the pending job. The added file gets no inherited cleanup approval. |
| Output/source changed | In review mode, replace a completed output with another file before requesting cleanup. Repeat with a different file at the original source path. | The app detects each identity change and leaves the replacement and original reference intact. |
| Failed or interrupted verification | Cancel or interrupt an attempt before verification completes. Also try cleanup on a sample and on a failed encode. | None is eligible. Retrying the encode creates a fresh attempt. |
| Unsupported Trash | On a volume for which recoverable trashing is unavailable, request cleanup of a disposable item. | The app explains the limitation and leaves the source in place. The valid output remains Completed. |

## Overnight and desktop sign-off

1. Start two queues with enough real footage to run for several hours. Include one known bad input. Use background priority, keep-awake and Keep originals. Record queue order, drive free space, OS power settings and start time. On a laptop use AC power with the lid open.
2. Use the computer briefly during encoding, then minimize the app and let the display sleep. In the morning inspect both tabs, the failure reason, final ratios, published files and remaining space. Independently play the first and last successful outputs. The queue should have continued past the bad item, and the app should release its sleep request once idle.
3. On Windows check 100%, 150% and 200% scaling. On macOS check Retina scaling. Resize the window, move between available displays, switch light/dark modes and navigate with the keyboard. Check that Start, Pause, the row expander, the preview controls and the cleanup choice remain visible, legible and reachable.
4. Record one result per OS, architecture and app build. Include the FFmpeg version, fixture, scenario, expected and observed behavior and a screenshot or job diagnostic for each failure. Distinguish **Passed**, **Failed**, **Blocked** and **Untested**. A working macOS run leaves Windows coverage open.

Treat source loss, overwriting an existing file, accepting a broken output as complete, processing a drop in the wrong queue, or a preview failure that kills a verified-supported archive run as release blockers. For a feature whose safety depends on the filesystem, report the tested volume type alongside its result.
