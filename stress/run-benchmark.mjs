#!/usr/bin/env node
/**
 * Bounded browser benchmark for the synthetic queue stress harness.
 * Uses the installed Google Chrome with a fresh, temporary profile per run.
 * No media files are created; all synthetic rows remain in browser memory.
 */
import { spawn, execFile } from 'node:child_process';
import { promisify } from 'node:util';
const execFileAsync = promisify(execFile);
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';

const sizes = new Set(['smoke', '10k', '50k']);
const size = process.argv[2] || 'smoke';
const baseUrl = process.argv[3] || 'http://127.0.0.1:4173/';
const timeoutMs = Number(process.env.STRESS_TIMEOUT_MS || 180_000);
if (!sizes.has(size)) throw new Error(`Usage: node stress/run-benchmark.mjs [smoke|10k|50k] [base-url]`);
if (!Number.isFinite(timeoutMs) || timeoutMs < 1_000) throw new Error('STRESS_TIMEOUT_MS must be at least 1000');

const chrome = process.env.CHROME_BIN || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const profile = await mkdtemp(join(tmpdir(), 'ffmpeg-queue-stress-'));
let browser;
let socket;
let id = 0;
const pending = new Map();

function send(method, params = {}) {
  const commandId = ++id;
  socket.send(JSON.stringify({ id: commandId, method, params }));
  return new Promise((resolve, reject) => pending.set(commandId, { resolve, reject }));
}

try {
  browser = spawn(chrome, [
    '--headless=new', '--disable-gpu', '--disable-extensions',
    '--no-first-run', '--no-default-browser-check', '--hide-scrollbars',
    '--remote-debugging-port=0', `--user-data-dir=${profile}`, 'about:blank'
  ], { stdio: 'ignore' });

  const activePort = join(profile, 'DevToolsActivePort');
  const start = Date.now();
  let port;
  while (Date.now() - start < 15_000) {
    try { port = (await readFile(activePort, 'utf8')).split('\n')[0]; break; }
    catch { await delay(100); }
  }
  if (!port) throw new Error('Chrome did not start its DevTools endpoint');
  const list = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
  const page = list.find(x => x.type === 'page');
  if (!page) throw new Error('Chrome did not expose a page target');
  socket = new WebSocket(page.webSocketDebuggerUrl);
  await new Promise((resolve, reject) => {
    socket.addEventListener('open', resolve, { once: true });
    socket.addEventListener('error', reject, { once: true });
  });
  socket.addEventListener('message', event => {
    const message = JSON.parse(event.data);
    if (message.id && pending.has(message.id)) {
      const { resolve, reject } = pending.get(message.id);
      pending.delete(message.id);
      message.error ? reject(new Error(message.error.message)) : resolve(message.result);
    }
  });
  await send('Page.enable');
  await send('Runtime.enable');
  await send('Performance.enable');
  const url = new URL(baseUrl);
  url.searchParams.set('size', size);
  await send('Page.navigate', { url: url.toString() });

  const evaluate = async (expression, awaitPromise = true) => {
    const result = await send('Runtime.evaluate', {
      expression, awaitPromise, returnByValue: true, timeout: timeoutMs
    });
    if (result.exceptionDetails) throw new Error(result.exceptionDetails.text || 'Browser evaluation failed');
    return result.result?.value;
  };
  await evaluate(`(async()=>{const end=Date.now()+${timeoutMs};while(Date.now()<end){const m=window.__stressHarness?.metrics?.();if(m?.status==='ready')return true;await new Promise(r=>setTimeout(r,25))}throw new Error('Harness did not become ready')})()`);
  const expectedCount = ({smoke:1000,'10k':10000,'50k':50000})[${JSON.stringify(size)}];
  const expectedBytes = 1_000_000_000_000;
  const baseline = await evaluate(`window.__stressHarness.metrics()`);
  if (baseline.fileCount !== expectedCount || baseline.bytes !== expectedBytes || baseline.uniquePhysicalBytes !== expectedBytes) throw new Error(`Bad fixture totals: ${JSON.stringify(baseline)}`);
  const base = { size, url: url.toString(), timeoutMs, expectedSyntheticBytes: expectedBytes, baseline };

  // Search sentinel through the app's real queue filter and validate the DOM.
  const search = await evaluate(`(async()=>{const h=window.__stressHarness;const f=h.frame.contentWindow;const q='SENTINEL-LAST-FILE-${String(1000).padStart(6,'0')}';const started=performance.now();const result=h.find(q);await new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r)));const d=f.document;const rows=[...d.querySelectorAll('#queue-rows tbody tr.tc-row')];const row=rows.find(r=>r.textContent.includes(q));return {query:q,result,elapsedMs:performance.now()-started,filteredRows:rows.length,visibleMatch:!!row,selected:row?.classList.contains('selected')||false,selectedId:row?.dataset.jobId||null,queueSearch:d.getElementById('enc-search-input')?.value||null}})()`);
  if (!search.visibleMatch || search.filteredRows !== 1 || !search.selected || search.selectedId !== 'stress-001000') throw new Error(`Sentinel search/selection validation failed: ${JSON.stringify(search)}`);
  base.searchValidation = search;

  // Clear the filter, scroll at head/middle/tail, and validate all queue rows return.
  const ranks = await evaluate(`(async()=>{const h=window.__stressHarness;const f=h.frame.contentWindow;const d=f.document;h.search('');await new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r)));const rows=[...d.querySelectorAll('#queue-rows tbody tr.tc-row')];const ranks=[0,Math.floor(rows.length/2),rows.length-1];const out=[];for(const i of ranks){const row=rows[i];const t=performance.now();row.scrollIntoView({block:'center'});await new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r)));out.push({rank:i,jobId:row.dataset.jobId,scrollMs:performance.now()-t,scrollTop:d.querySelector('.transcode-table-wrap')?.scrollTop??null})}return {restoredRows:rows.length,searchValue:d.getElementById('enc-search-input')?.value||null,ranks:out,tailJobId:rows.at(-1)?.dataset.jobId}})()`);
  if (ranks.restoredRows !== expectedCount || ranks.tailJobId !== 'stress-001000') throw new Error(`Filter clear/full queue validation failed: ${JSON.stringify(ranks)}`);
  base.rankScroll = ranks;
  const nativeFind = await evaluate(`(()=>{const f=window.__stressHarness.frame.contentWindow;const d=f.document;const q='SENTINEL-LAST-FILE-001000';const found=f.find(q);return {found,searchValue:d.getElementById('enc-search-input')?.value||null,bodyHasSentinel:d.body.innerText.includes(q)}})()`);
  base.nativeFind = nativeFind;

  // Record queue-row identity and status-cell mutations during progress events.
  const churn = await evaluate(`(async()=>{const h=window.__stressHarness;const f=h.frame.contentWindow;const d=f.document;h.search('');await new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r)));const before=[...d.querySelectorAll('#queue-rows tbody tr.tc-row')];const target=before.find(r=>r.dataset.jobId==='stress-000038');const beforeHtml=target?.querySelector('[data-status-cell]')?.innerHTML;let childMutations=0,statusMutations=0;const observer=new MutationObserver(records=>{for(const m of records){if(m.type==='childList')childMutations++;if(m.target.closest?.('[data-status-cell]'))statusMutations++}});observer.observe(d.querySelector('#queue-rows'),{subtree:true,childList:true,characterData:true});h.setScenario('progress');await new Promise(r=>setTimeout(r,650));h.stop();observer.disconnect();const after=[...d.querySelectorAll('#queue-rows tbody tr.tc-row')];const sameRows=before.length===after.length&&before.every((r,i)=>r===after[i]);const afterHtml=target?.querySelector('[data-status-cell]')?.innerHTML;return {rowCountBefore:before.length,rowCountAfter:after.length,sameRowNodes:sameRows,progressDomChanged:beforeHtml!==afterHtml,childMutations,statusMutations,metrics:h.metrics()}})()`);
  if (!churn.sameRowNodes || !churn.progressDomChanged || churn.rowCountAfter !== expectedCount) throw new Error(`Progress churn validation failed: ${JSON.stringify(churn)}`);
  base.progressChurn = churn;
  base.environment = await evaluate(`({userAgent:navigator.userAgent,platform:navigator.platform,hardwareConcurrency:navigator.hardwareConcurrency,deviceMemory:navigator.deviceMemory||null,viewport:{width:innerWidth,height:innerHeight},dpr:devicePixelRatio})`);
  base.browserProcessRss = await (async()=>{try {const {stdout}=await execFileAsync('ps',['-axo','rss=,command=']);return stdout.split('\n').filter(x=>x.includes(profile)).map(x=>Number(x.trim().split(/\s+/,1)[0])).filter(Number.isFinite).reduce((a,b)=>a+b,0)}catch{return null}})();
  base.performanceMetrics = (await send('Performance.getMetrics')).metrics;
  console.log(JSON.stringify(base, null, 2));
} finally {
  try { socket?.close(); } catch {}
  try { browser?.kill('SIGTERM'); } catch {}
  await delay(250);
  try { browser?.kill('SIGKILL'); } catch {}
  await rm(profile, { recursive: true, force: true });
}
