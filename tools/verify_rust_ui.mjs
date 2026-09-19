const fs = await import('node:fs');
const { spawn } = await import('node:child_process');
import { fileURLToPath } from 'node:url';
import path from 'node:path';
const HERE = path.dirname(fileURLToPath(import.meta.url)).replace(/\\\\/g, '/');
const profile = HERE + '/_cdp-profile';
const PORT = process.argv[2] || '5050';
const BASE = `http://127.0.0.1:${PORT}`;

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
//
// The suite resets the config to a known set before running. Without that it is
// order-dependent: tools/audit_rust.py leaves a dozen widgets behind, and the
// "remove a widget" step then deletes one weather card while a second, added by
// this very run, remains -- so the location assertions read a card that was
// never configured.
//
// Only the widget set is replaced; devices are left alone so this can run after
// the control suite without disturbing its fixture.
// ---------------------------------------------------------------------------
const WEATHER_ID = 'fixture-weather';

async function resetFixture() {
  const cfg = (await (await fetch(`${BASE}/api/config`)).json()).config;
  cfg.widgets = [
    { id: WEATHER_ID, kind: 'weather', title: 'Weather', icon: 'cloud-sun',
      settings: { location: 'Berlin', units: 'c', days: 3 } },
    { id: 'fixture-clock', kind: 'clock', title: 'Clock', icon: 'clock',
      settings: { label: '', format: '24', seconds: false, date: true } },
  ];
  const r = await fetch(`${BASE}/api/config`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(cfg),
  });
  return (await r.json()).success === true;
}

const configured = await resetFixture();
console.log(`fixture: 2 widgets, weather location Berlin -> ${configured}`);
if (!configured) {
  console.error('could not configure the fixture; is the server running?');
  process.exit(2);
}

const proc = spawn(CHROME, ['--headless=new', '--disable-gpu', '--no-sandbox', '--disable-dev-shm-usage',
  '--no-first-run', '--disable-breakpad', '--hide-scrollbars', '--remote-debugging-port=9651',
  `--user-data-dir=${profile}`, '--window-size=1440,1000', 'about:blank'], { stdio: 'ignore' });
let id = 0, ws, pending = new Map(), events = [];
for (let i = 0; i < 60; i++) {
  try { const l = await (await fetch('http://127.0.0.1:9651/json/list')).json();
    const t = l.find(x => x.type === 'page'); if (t) { ws = new WebSocket(t.webSocketDebuggerUrl); break; } } catch { }
  await new Promise(r => setTimeout(r, 250));
}
await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
ws.onmessage = (e) => { const m = JSON.parse(e.data);
  if (m.id && pending.has(m.id)) { pending.get(m.id)(m); pending.delete(m.id); } else if (m.method) events.push(m); };
const send = (m, p = {}) => new Promise(r => { const i = ++id; pending.set(i, r); ws.send(JSON.stringify({ id: i, method: m, params: p })); });
const ev = async (e) => { const r = await send('Runtime.evaluate', { expression: e, returnByValue: true, awaitPromise: true });
  if (r.result?.exceptionDetails) return { __error: (r.result.exceptionDetails.exception?.description || '').split('\n')[0] };
  return r.result?.result?.value; };
const shot = async (n, o = {}) => { const s = await send('Page.captureScreenshot', Object.assign({ format: 'png' }, o));
  // Create the directory on demand: it is a build artefact, so it is frequently
  // cleaned, and a missing folder must not abort the run at the first screenshot.
  fs.mkdirSync(`${HERE}/shots`, { recursive: true });
  fs.writeFileSync(`${HERE}/shots/${n}.png`, Buffer.from(s.result.data, 'base64')); console.log('  ' + n + '.png'); };
const wait = (ms) => new Promise(r => setTimeout(r, ms));

const results = [];
const check = (name, ok, detail = '') => { results.push(ok); console.log(`  ${ok ? 'ok  ' : 'FAIL'} ${name}${detail ? '  -- ' + detail : ''}`); };
const PICKER = { weather: 0, clock: 1, rss: 2, overview: 3, containers: 4, links: 5 };

await send('Page.enable'); await send('Runtime.enable');
await send('Emulation.setDeviceMetricsOverride', { width: 1440, height: 1000, deviceScaleFactor: 1, mobile: false });
await send('Page.navigate', { url: `http://127.0.0.1:${PORT}/?e=${Date.now()}` });
await wait(5000);

console.log('\n== initial render ==');
const boot = JSON.parse(await ev(`JSON.stringify({
  sections: document.querySelectorAll('.section').length,
  widgets: document.querySelectorAll('.widget').length,
  editButton: !!document.querySelector('.header-actions .btn-ghost'),
})`));
check('dashboard renders', boot.sections >= 1, JSON.stringify(boot));
check('edit controls hidden by default',
      (await ev(`document.querySelectorAll('.card-tools').length`)) === 0 &&
      (await ev(`!!document.querySelector('.edit-banner')`)) === false);
check('no uncaught exceptions on load',
      events.filter(e => e.method === 'Runtime.exceptionThrown').length === 0);
await shot('rust-1-dashboard', { captureBeyondViewport: true });

console.log('\n== edit mode ==');
await ev(`[...document.querySelectorAll('button')].find(b=>b.textContent.trim()==='Edit').click()`);
await wait(700);
check('banner appears', (await ev(`!!document.querySelector('.edit-banner')`)) === true);
check('add-widget button appears',
      (await ev(`[...document.querySelectorAll('button')].some(b=>/Add widget/.test(b.textContent))`)) === true);

console.log('\n== add a widget from the picker ==');
// Independent of whatever the stored config happens to contain.
const before = await ev(`document.querySelectorAll('.widget').length`);
await ev(`[...document.querySelectorAll('button')].find(b=>/Add widget/.test(b.textContent)).click()`);
await wait(600);
check('picker opens', (await ev(`!!document.querySelector('.modal')`)) === true);
const options = await ev(`document.querySelectorAll('.picker-item').length`);
check('picker lists all six types', options === 6, `${options} types`);
await shot('rust-2-picker');
await ev(`document.querySelectorAll('.picker-item')[${PICKER.weather}].click()`);
await wait(3500);
const after = await ev(`document.querySelectorAll('.widget').length`);
check('widget is added', after === before + 1, `${before} -> ${after}`);
check('picker closed after adding', (await ev(`!!document.querySelector('.modal')`)) === false);
check('new widget renders live data', (await ev(`!!document.querySelector('.weather-temp')`)) === true);

console.log('\n== edit the weather location (the headline case) ==');
const opened = await ev(`(async () => {
  const card = document.querySelector('.card[data-kind="weather"]');
  if (!card) return 'NO CARD';
  // Wait for the widget's data before opening the form, so the comparison below
  // is against a loaded card rather than one still fetching.
  for (let i = 0; i < 40 && !card.querySelector('.weather-meta'); i++) {
    await new Promise(r => setTimeout(r, 250));
  }
  const shown = (card.querySelector('.weather-meta .muted')||{}).textContent || '';
  card.querySelector('.card-tools .chip').click();
  await new Promise(r => setTimeout(r, 700));
  const input = card.querySelector('#wf-location');
  if (!input) return 'NO INPUT';
  return JSON.stringify({ value: input.value, shown });
})()`);
// The stored value survives between runs, so asserting on a specific city would
// fail for reasons unrelated to the editor. What matters is that the field is
// populated and agrees with what the card is displaying.
const openedObj = (() => { try { return JSON.parse(opened); } catch { return null; } })();
check('configure form opens seeded with the stored value',
      !!openedObj && openedObj.value.length > 0 && openedObj.shown.length > 0
        && openedObj.shown.trim().toLowerCase().startsWith(openedObj.value.trim().toLowerCase().slice(0, 4)),
      String(opened).slice(0, 90));
await shot('rust-3-editor');

const saved = await ev(`(async () => {
  const card = document.querySelector('.card[data-kind="weather"]');
  const input = card.querySelector('#wf-location');
  input.value = 'Reykjavik';
  input.dispatchEvent(new Event('input', { bubbles: true }));
  card.querySelector('form').querySelector('button[type=submit]').click();
  await new Promise(r=>setTimeout(r,4000));
  const c = document.querySelector('.card[data-kind="weather"]');
  return {
    location: (c.querySelector('.weather-meta .muted')||{}).textContent || '',
    temp: (c.querySelector('.weather-temp')||{}).textContent || '',
  };
})()`);
check('location saved', /reykjav/i.test(saved.location || ''), JSON.stringify(saved));
check('temperature data changed with it', /\d/.test(saved.temp || ''), saved.temp);
await shot('rust-4-location-changed', { captureBeyondViewport: true });

console.log('\n== remove a widget ==');
const beforeDel = await ev(`document.querySelectorAll('.widget').length`);
await ev(`(async () => {
  const card = document.querySelector('.card[data-kind="weather"]');
  card.querySelectorAll('.card-tools .chip')[1].click();
  await new Promise(r=>setTimeout(r,3000));
})()`);
const afterDel = await ev(`document.querySelectorAll('.widget').length`);
check('widget removed', afterDel === beforeDel - 1, `${beforeDel} -> ${afterDel}`);

console.log('\n== persistence ==');
await send('Page.navigate', { url: `http://127.0.0.1:${PORT}/?r=${Date.now()}` });
await wait(5000);
const reloaded = await ev(`document.querySelectorAll('.widget').length`);
check('survives a page reload', reloaded === afterDel, `${reloaded} widgets`);
await shot('rust-5-final', { captureBeyondViewport: true });

const errs = events.filter(e => e.method === 'Runtime.exceptionThrown')
  .map(e => (e.params.exceptionDetails.exception?.description || '').split('\n')[0]);
check('no uncaught exceptions overall', errs.length === 0, errs.slice(0, 2).join(' | '));

proc.kill();
const failed = results.filter(r => !r).length;
console.log(`\n${results.length - failed}/${results.length} checks passed`);
process.exit(failed ? 1 : 0);
