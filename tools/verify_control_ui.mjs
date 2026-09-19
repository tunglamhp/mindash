/**
 * UI verification for the control panels: devices, live stats, file browser,
 * containers.
 *
 * The suite configures its own fixture through the API rather than assuming what
 * is stored. Without that it degenerates quietly: with no device configured it
 * clicks buttons that are not there and reports failures that are really missing
 * setup.
 *
 * The fixture device points at a loopback address, so the server reads it
 * locally. Remote access over SSH is covered by tools/audit_control.py, which
 * needs a real Linux target; keeping that out of here means this suite runs on
 * any machine.
 *
 * Usage: node tools/verify_control_ui.mjs [port]
 */
const fs = await import('node:fs');
const { spawn } = await import('node:child_process');

import { fileURLToPath } from 'node:url';
import path from 'node:path';
const HERE = path.dirname(fileURLToPath(import.meta.url)).replace(/\\\\/g, '/');
const PORT = process.argv[2] || '5050';
const BASE = `http://127.0.0.1:${PORT}`;
const DEVICE = 'uitest';

/**
 * Find a Chrome binary.
 *
 * Checked in order: an explicit CHROME override, then the usual locations per
 * platform. Hardcoding the Windows path meant this suite could only ever run on
 * the machine it was written on, and failed in CI with ENOENT.
 */
function findChrome() {
  if (process.env.CHROME && fs.existsSync(process.env.CHROME)) return process.env.CHROME;
  const candidates = [
    'C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe',
    'C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe',
    '/usr/bin/google-chrome',
    '/usr/bin/google-chrome-stable',
    '/usr/bin/chromium',
    '/usr/bin/chromium-browser',
    '/snap/bin/chromium',
    '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
    '/Applications/Chromium.app/Contents/MacOS/Chromium',
  ];
  for (const c of candidates) {
    if (fs.existsSync(c)) return c;
  }
  throw new Error(
    'no Chrome found. Set CHROME=/path/to/chrome. Looked in:\n  ' +
      candidates.join('\n  ')
  );
}

const CHROME = findChrome();

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------
const demoDir = `${HERE}/_demo`;
// Root the browser directly at the fixture. That keeps the assertions simple and
// makes the confinement check meaningful: the parent of this root is a real
// directory the browser must refuse to show.
const ROOT = demoDir.replace(/\\/g, '/');
const EXPECTED_ENTRIES = 3; // subdir + readme.txt + notes.md

fs.mkdirSync(`${demoDir}/subdir`, { recursive: true });
fs.writeFileSync(`${demoDir}/readme.txt`, 'MinDash file browser fixture\nsecond line\n');
fs.writeFileSync(`${demoDir}/notes.md`, '# notes\n');

async function configure() {
  const cfg = (await (await fetch(`${BASE}/api/config`)).json()).config;
  const host = cfg.devices.find(d => d.is_host);
  const others = cfg.devices.filter(d => !d.is_host && d.id !== DEVICE);
  cfg.devices = [
    host,
    {
      id: DEVICE,
      name: 'UI Test Host',
      ip: '127.0.0.1',
      is_host: false,
      ssh: { user: 'root', port: 22 },
      files_root: ROOT,
      wol: { mac: 'aa:bb:cc:dd:ee:ff', broadcast: '127.0.0.1' },
      docker: { containers: [] },
    },
    ...others,
  ].filter(Boolean);
  const r = await fetch(`${BASE}/api/config`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(cfg),
  });
  return (await r.json()).success === true;
}

const configured = await configure();
console.log(`fixture: device "${DEVICE}" rooted at ${ROOT} -> ${configured}`);
if (!configured) {
  console.error('could not configure the fixture; is the server running?');
  process.exit(2);
}

// ---------------------------------------------------------------------------
// Browser
// ---------------------------------------------------------------------------
const proc = spawn(CHROME, ['--headless=new', '--disable-gpu', '--no-sandbox', '--disable-dev-shm-usage',
  '--no-first-run', '--disable-breakpad', '--hide-scrollbars', '--remote-debugging-port=9691',
  `--user-data-dir=${HERE}/_cdp-profile`, '--window-size=1440,1000', 'about:blank'], { stdio: 'ignore' });

let id = 0, ws, pending = new Map(), events = [];
for (let i = 0; i < 60; i++) {
  try {
    const list = await (await fetch('http://127.0.0.1:9691/json/list')).json();
    const page = list.find(x => x.type === 'page');
    if (page) { ws = new WebSocket(page.webSocketDebuggerUrl); break; }
  } catch { /* not up yet */ }
  await new Promise(r => setTimeout(r, 250));
}
await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
ws.onmessage = (e) => {
  const m = JSON.parse(e.data);
  if (m.id && pending.has(m.id)) { pending.get(m.id)(m); pending.delete(m.id); }
  else if (m.method) events.push(m);
};
const send = (m, p = {}) => new Promise(r => {
  const i = ++id; pending.set(i, r); ws.send(JSON.stringify({ id: i, method: m, params: p }));
});
const ev = async (expr) => {
  const r = await send('Runtime.evaluate', { expression: expr, returnByValue: true, awaitPromise: true });
  if (r.result?.exceptionDetails) {
    return 'EXC: ' + (r.result.exceptionDetails.exception?.description || '').split('\n')[0];
  }
  return r.result?.result?.value;
};
const shot = async (name, opts = {}) => {
  const s = await send('Page.captureScreenshot', Object.assign({ format: 'png' }, opts));
  // Created on demand: the directory is a build artefact and is frequently
  // cleaned, and a missing folder must not abort the run at the first screenshot.
  fs.mkdirSync(`${HERE}/shots`, { recursive: true });
  fs.writeFileSync(`${HERE}/shots/${name}.png`, Buffer.from(s.result.data, 'base64'));
  console.log(`  ${name}.png`);
};
const wait = (ms) => new Promise(r => setTimeout(r, ms));

const results = [];
const check = (name, ok, detail = '') => {
  results.push(ok);
  console.log(`  ${ok ? 'ok  ' : 'FAIL'} ${name}${detail ? '  -- ' + detail : ''}`);
};

/** Safe parse: a probe that errored must fail its check, not crash the suite. */
function asJson(value) {
  if (typeof value !== 'string') return null;
  if (!value.startsWith('[') && !value.startsWith('{')) return null;
  try { return JSON.parse(value); } catch { return null; }
}

const CARD = `[...document.querySelectorAll('.card.device')].find(c=>!c.querySelector('.pill'))`;
const HOST_CARD = `[...document.querySelectorAll('.card.device')].find(c=>c.querySelector('.pill'))`;

const clickChip = (cardExpr, label) => ev(`(() => {
  const c = ${cardExpr};
  if (!c) return 'NO CARD';
  const chip = [...c.querySelectorAll('.chip')].find(b => b.textContent === ${JSON.stringify(label)});
  if (!chip) return 'NO CHIP';
  chip.click();
  return 'ok';
})()`);

await send('Page.enable');
await send('Runtime.enable');
await send('Emulation.setDeviceMetricsOverride', { width: 1440, height: 1000, deviceScaleFactor: 1, mobile: false });
await send('Page.navigate', { url: `${BASE}/?c=${Date.now()}` });
await wait(5500);

console.log('\n== tabs ==');
const tabCount = await ev(`document.querySelectorAll('.tab').length`);
check('tab strip renders', tabCount === 3, `${tabCount} tabs`);
check('dashboard is the default tab',
      await ev(`document.querySelector('.tab.active')?.textContent`) === 'Dashboard');
check('no uncaught exceptions on load',
      events.filter(e => e.method === 'Runtime.exceptionThrown').length === 0);
await shot('ctl-1-dashboard', { captureBeyondViewport: true });

console.log('\n== devices tab ==');
await ev(`[...document.querySelectorAll('.tab')].find(t=>t.textContent==='Devices').click()`);
await wait(3000);
const cards = await ev(`document.querySelectorAll('.card.device').length`);
check('device cards render', cards >= 2, `${cards} cards`);
check('host device is marked', await ev(`!!document.querySelector('.card.device .pill')`));
check('host has no power buttons',
      (await ev(`(() => {
        const c = ${HOST_CARD};
        return c ? [...c.querySelectorAll('.chip')].some(b => /Shut down|Reboot|Suspend/.test(b.textContent)) : null;
      })()`)) === false);
const fixtureChips = await ev(`(() => {
  const c = ${CARD};
  return c ? JSON.stringify([...c.querySelectorAll('.chip')].map(b => b.textContent)) : null;
})()`);
const chips = asJson(fixtureChips) || [];
check('fixture device offers control actions',
      ['Reboot', 'Suspend', 'Shut down', 'Wake', 'Files'].every(a => chips.includes(a)),
      JSON.stringify(chips));
await shot('ctl-2-devices', { captureBeyondViewport: true });

console.log('\n== live stats ==');
check('stats chip clicked', (await clickChip(CARD, 'Load stats')) === 'ok');
await wait(6000);
const statsRaw = await ev(`(() => {
  const c = ${CARD};
  if (!c) return null;
  return JSON.stringify({
    stats: [...c.querySelectorAll('.stat')].map(s => s.textContent.trim()),
    rows: c.querySelectorAll('.rows .row').length,
    error: c.querySelector('.card-error')?.textContent || '',
  });
})()`);
const stats = asJson(statsRaw);
check('stats load into the card',
      !!stats && stats.stats.length >= 3 && !/—/.test(stats.stats.join('')),
      stats ? stats.stats.join(' | ').slice(0, 110) : String(statsRaw));
check('disk rows render', !!stats && stats.rows > 0, stats ? `${stats.rows} rows` : '');
check('no error in the card', !!stats && !stats.error, stats?.error || '');
await shot('ctl-3-stats', { captureBeyondViewport: true });

console.log('\n== file browser ==');
check('files chip clicked', (await clickChip(CARD, 'Files')) === 'ok');
await wait(4000);

// The browser opens at the configured root, so the fixture is already on screen.
const opened = await ev(`(() => {
  const b = document.querySelector('.browser');
  if (!b) return null;
  return JSON.stringify({
    rows: b.querySelectorAll('.browser-list .row').length,
    crumbs: b.querySelectorAll('.crumb').length,
    names: [...b.querySelectorAll('.row-name')].map(n => n.textContent.trim()),
    hasUp: !![...b.querySelectorAll('.chip')].find(c => c.textContent === 'Up'),
    error: b.querySelector('.card-error')?.textContent || '',
  });
})()`);
const list = asJson(opened);
check('fixture directory lists its contents', !!list && list.rows === EXPECTED_ENTRIES,
      list ? `${list.rows} rows: ${list.names.join(', ')}` : String(opened));
check('no error in the browser', !!list && !list.error, list?.error || '');
check('directories sort first',
      (await ev(`(() => {
        const rows = [...document.querySelectorAll('.browser-list .row')];
        if (!rows.length) return false;
        // The marker slot is on every row; only directories get a glyph in it.
        return rows[0].querySelector('.row-mark') !== null
            && rows.slice(1).every(r => r.querySelector('.row-mark') === null);
      })()`)) === true);
// At the configured root there is nowhere legal to go up to, so the browser must
// not offer it -- and the server refuses the parent path anyway.
check('no Up button at the configured root', !!list && list.hasUp === false,
      list ? `hasUp=${list.hasUp}` : String(opened));
await shot('ctl-4-files', { captureBeyondViewport: true });

console.log('\n== open a file ==');
const preview = await ev(`(async () => {
  const rows = [...document.querySelectorAll('.browser-list .row')];
  // The known file, not whichever happens to sort first.
  const file = rows.find(r => r.textContent.includes('readme.txt'));
  if (!file) return 'NO FILE ROW';
  file.click();
  await new Promise(r => setTimeout(r, 3000));
  const p = document.querySelector('.preview-body');
  return p ? p.textContent : 'NO PREVIEW';
})()`);
check('file preview opens with its contents',
      typeof preview === 'string' && /fixture/.test(preview) && /second line/.test(preview),
      JSON.stringify(String(preview).slice(0, 48)));
await shot('ctl-5-preview', { captureBeyondViewport: true });

console.log('\n== navigate down and back up ==');
const nav = await ev(`(async () => {
  const dir = [...document.querySelectorAll('.browser-list .row')].find(r => r.querySelector('.row-mark'));
  if (!dir) return 'NO DIR';
  dir.click();
  await new Promise(r => setTimeout(r, 3000));
  const crumbsIn = document.querySelectorAll('.crumb').length;
  const up = [...document.querySelectorAll('.browser-actions .chip')].find(b => b.textContent === 'Up');
  if (!up) return 'NO UP BUTTON';
  up.click();
  await new Promise(r => setTimeout(r, 3000));
  const b = document.querySelector('.browser');
  return JSON.stringify({
    crumbsIn,
    crumbsAfterUp: b.querySelectorAll('.crumb').length,
    rowsAfterUp: b.querySelectorAll('.browser-list .row').length,
  });
})()`);
const navObj = asJson(nav);
check('descending adds a breadcrumb', !!navObj && navObj.crumbsIn === list.crumbs + 1,
      String(nav).slice(0, 40));
check('an Up button appears below the root', typeof nav === 'string' && !nav.startsWith('NO'),
      String(nav).slice(0, 40));
check('going up returns to the same listing',
      !!navObj && navObj.rowsAfterUp === EXPECTED_ENTRIES && navObj.crumbsAfterUp === list.crumbs,
      navObj ? `${navObj.rowsAfterUp} rows, ${navObj.crumbsAfterUp} crumbs` : '');

console.log('\n== containers tab ==');
await ev(`[...document.querySelectorAll('.tab')].find(t=>t.textContent==='Containers').click()`);
await wait(3000);
const contRaw = await ev(`(() => {
  const rows = document.querySelectorAll('.container-card .row');
  const buttons = [...new Set([...document.querySelectorAll('.container-card .chip')].map(b => b.textContent))];
  return JSON.stringify({ rows: rows.length, buttons });
})()`);
const cont = asJson(contRaw) || { rows: -1, buttons: [] };
check('container rows render', cont.rows >= 0, `${cont.rows} rows`);
check('running containers offer Stop and Restart',
      cont.rows === 0 || (cont.buttons.includes('Stop') && cont.buttons.includes('Restart')),
      JSON.stringify(cont.buttons));
await shot('ctl-6-containers', { captureBeyondViewport: true });

console.log('\n== console ==');
const errs = events.filter(e => e.method === 'Runtime.exceptionThrown')
  .map(e => (e.params.exceptionDetails.exception?.description || '').split('\n')[0]);
check('no uncaught exceptions overall', errs.length === 0, errs.slice(0, 2).join(' | '));

proc.kill();
fs.rmSync(`${HERE}/_demo`, { recursive: true, force: true });

const failed = results.filter(r => !r).length;
console.log(`\n${results.length - failed}/${results.length} checks passed`);
process.exit(failed ? 1 : 0);
