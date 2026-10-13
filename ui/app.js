// VillFlow settings window: vanilla JS over Tauri commands (withGlobalTauri).
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (sel, root = document) => root.querySelector(sel);
const $all = (sel, root = document) => [...root.querySelectorAll(sel)];

let state = null; // { settings, stats, providers, loaded_provider, hotkey_error, data_dir, version }
const status = {}; // provider id -> last status from its page
const PAGE = 40;
const hist = { offset: 0, query: '', kind: '', loading: false };

// ---------- icons (one stroke style everywhere) ----------
const ICONS = {
  home: '<path d="M15 21v-8a1 1 0 0 0-1-1h-4a1 1 0 0 0-1 1v8"/><path d="M3 10a2 2 0 0 1 .7-1.5l7-6a2 2 0 0 1 2.6 0l7 6A2 2 0 0 1 21 10v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>',
  history: '<path d="M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8"/><path d="M3 3v5h5"/><path d="M12 7v5l4 2"/>',
  book: '<path d="M12 7v14"/><path d="M3 18a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1h5a4 4 0 0 1 4 4 4 4 0 0 1 4-4h5a1 1 0 0 1 1 1v13a1 1 0 0 1-1 1h-6a3 3 0 0 0-3 3 3 3 0 0 0-3-3z"/>',
  user: '<circle cx="12" cy="12" r="10"/><circle cx="12" cy="10" r="3"/><path d="M7 20.66V19a2 2 0 0 1 2-2h6a2 2 0 0 1 2 2v1.66"/>',
  sliders: '<path d="M14 17H5"/><path d="M19 7h-9"/><circle cx="17" cy="17" r="3"/><circle cx="7" cy="7" r="3"/>',
  copy: '<rect width="14" height="14" x="8" y="8" rx="2"/><path d="M4 16a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h10a2 2 0 0 1 2 2"/>',
  trash: '<path d="M3 6h18"/><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6"/><path d="M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/>',
  search: '<circle cx="11" cy="11" r="7"/><path d="m20 20-3.5-3.5"/>',
  arrow: '<path d="M5 12h14"/><path d="m13 6 6 6-6 6"/>',
  check: '<path d="M20 6 9 17l-5-5"/>',
  alert: '<circle cx="12" cy="12" r="10"/><path d="M12 8v4"/><path d="M12 16h.01"/>',
};
const icon = (name) => `<svg class="ico" viewBox="0 0 24 24" aria-hidden="true">${ICONS[name] || ''}</svg>`;
function hydrateIcons(root = document) {
  $all('i[data-icon]', root).forEach((el) => { if (!el.firstChild) el.innerHTML = icon(el.dataset.icon); });
}

// ---------- helpers ----------
function toast(msg, opts = {}) {
  const t = $('#toast');
  $('span', t).textContent = msg;
  $('.toast-icon', t).innerHTML = icon(opts.error ? 'alert' : 'check');
  t.classList.toggle('err', !!opts.error);
  t.classList.add('show');
  clearTimeout(toast.timer);
  toast.timer = setTimeout(() => t.classList.remove('show'), opts.error ? 4500 : 2200);
}

function confirmDialog(title, text, okLabel) {
  const d = $('#confirmDialog');
  $('#confirmTitle').textContent = title;
  $('#confirmText').textContent = text;
  $('#confirmOk').textContent = okLabel;
  d.returnValue = '';
  d.showModal();
  return new Promise((resolve) => d.addEventListener('close', () => resolve(d.returnValue === 'ok'), { once: true }));
}

const esc = (s) => String(s).replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
const keysHtml = (combo) => combo.split('+').map((k) => `<kbd>${esc(k.trim())}</kbd>`).join('');
const fmtNum = (n) => Number(n || 0).toLocaleString();
const plural = (n, one, many = `${one}s`) => `${fmtNum(n)} ${n === 1 ? one : many}`;

function fmtDuration(secs) {
  secs = Math.max(0, Math.round(secs));
  if (secs < 60) return `${secs}s`;
  const m = Math.floor(secs / 60);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  return h < 24 ? `${h}h ${m % 60}m` : `${Math.floor(h / 24)}d ${h % 24}h`;
}

const sameDay = (a, b) => a.toDateString() === b.toDateString();
function fmtTime(ts, withDay) {
  const d = new Date(ts);
  const time = d.toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' });
  if (!withDay || sameDay(d, new Date())) return time;
  return d.toLocaleDateString([], { month: 'short', day: 'numeric' });
}

function dayLabel(ts) {
  const d = new Date(ts);
  if (sameDay(d, new Date())) return 'Today';
  if (sameDay(d, new Date(Date.now() - 86400000))) return 'Yesterday';
  const opts = { weekday: 'long', month: 'long', day: 'numeric' };
  if (d.getFullYear() !== new Date().getFullYear()) opts.year = 'numeric';
  return d.toLocaleDateString([], opts);
}

function providerName(id) {
  const p = state?.providers.find((x) => x.id === id);
  return p ? p.name : id ? id[0].toUpperCase() + id.slice(1) : '';
}

async function call(cmd, args) {
  try {
    return await invoke(cmd, args);
  } catch (e) {
    toast(String(e), { error: true });
    throw e;
  }
}

// Roles: who dictates, who writes, and whether writing is used at all.
const commandOn = () => !!state.settings.command_hotkey;
const writerInUse = () => commandOn() || state.settings.polish;
function writerId() {
  const w = state.settings.writer;
  return !w || w === 'same' ? state.settings.provider : w;
}
function inUse(id) {
  return id === state.settings.provider || (writerInUse() && id === writerId());
}

// ---------- navigation ----------
function show(view) {
  if (!document.getElementById(`view-${view}`)) return;
  $all('.nav-item').forEach((b) => {
    const on = b.dataset.view === view;
    b.classList.toggle('active', on);
    if (on) b.setAttribute('aria-current', 'page'); else b.removeAttribute('aria-current');
  });
  $all('.view').forEach((v) => v.classList.toggle('active', v.id === `view-${view}`));
  $('main').scrollTop = 0;
  if (view === 'history') loadHistory(true);
  if (view === 'providers') { renderAccounts(); checkAll(); }
}
$all('.nav-item').forEach((b) => b.addEventListener('click', () => show(b.dataset.view)));
document.addEventListener('click', (ev) => {
  const g = ev.target.closest('[data-goto]');
  if (g) show(g.dataset.goto);
});
document.addEventListener('keydown', (ev) => {
  if (ev.ctrlKey && ev.key.toLowerCase() === 'f') {
    ev.preventDefault();
    show('history');
    $('#search').focus();
  }
});

// ---------- state ----------
async function refresh() {
  state = await call('get_state');
  renderChrome();
  renderHome();
  renderSettings();
  renderDictionary();
  if ($('#view-providers').classList.contains('active')) renderAccounts();
}

// What a status means, in words, plus a dot class.
function describeStatus(id) {
  const st = status[id];
  if (!inUse(id)) return { dot: '', text: 'Not in use' };
  if (!st) return { dot: '', text: 'Not checked yet' };
  if (st.checking) return { dot: 'warn', text: 'Checking…' };
  if (st.error) return { dot: 'bad', text: st.error };
  if (st.ready) return { dot: 'ok', text: 'Signed in and ready' };
  if (st.signed_out) return { dot: 'bad', text: 'Not signed in' };
  if (!st.composer) return { dot: 'warn', text: 'Page is still loading' };
  return { dot: 'warn', text: 'Microphone button not found' };
}

function renderChrome() {
  const s = state.settings;
  // Sidebar: the dictation provider's state, or the writer's if that one needs attention.
  const ids = [s.provider, ...(writerInUse() && writerId() !== s.provider ? [writerId()] : [])];
  const bad = ids.find((id) => status[id] && (status[id].signed_out || status[id].error));
  const id = bad || s.provider;
  const st = status[id];
  const d = describeStatus(id);
  $('#statusDot').className = `dot ${d.dot}`;
  $('#statusText').textContent = !st ? `${providerName(id)}`
    : st.checking ? `Checking ${providerName(id)}…`
    : st.ready ? `${providerName(id)} ready`
    : st.signed_out ? `Sign in to ${providerName(id)}`
    : `${providerName(id)}: ${d.text.toLowerCase()}`;

  renderHomeHead();
}

const inUseIds = () => {
  const s = state.settings;
  return [s.provider, ...(writerInUse() && writerId() !== s.provider ? [writerId()] : [])];
};

// Home headline: what state VillFlow is in, and the one action that fixes it.
function renderHomeHead() {
  const s = state.settings;
  const ids = inUseIds();
  const who = (id) => `<span class="who">${esc(providerName(id))}</span>`;
  $('#homeLede').innerHTML = !writerInUse()
    ? `${who(s.provider)} listens.`
    : writerId() === s.provider
      ? `${who(s.provider)} listens and handles commands.`
      : `${who(s.provider)} listens, ${who(writerId())} handles commands.`;
  const out = ids.find((id) => status[id]?.signed_out);
  const broken = ids.find((id) => status[id] && !status[id].checking && !status[id].ready && !status[id].signed_out);
  const pending = ids.some((id) => !status[id] || status[id].checking);
  let title = 'Ready to dictate';
  let action = '';
  if (state.hotkey_error) {
    title = 'Your hotkey isn’t working';
    $('#homeLede').textContent = `${state.hotkey_error}. Pick a different one, or restart VillFlow.`;
    action = '<button class="btn primary" data-goto="settings">Change hotkey</button>';
  } else if (!s.onboarded && (out === s.provider || pending)) {
    title = 'Finish setting up';
  } else if (out) {
    title = `Sign in to ${providerName(out)}`;
    action = `<button class="btn primary" data-signin="${esc(out)}">Open ${esc(providerName(out))}</button>`;
  } else if (broken) {
    title = `${providerName(broken)} isn’t ready`;
    action = '<button class="btn" data-goto="providers">Check accounts</button>';
  } else if (pending) {
    title = 'Getting ready…';
  }
  $('#homeTitle').textContent = title;
  $('#homeAction').innerHTML = action;
}

document.addEventListener('click', (ev) => {
  const b = ev.target.closest('[data-signin]');
  if (b) call('provider_open', { id: b.dataset.signin });
});

const OUTPUT_TEXT = { paste: 'Pasted at the cursor', type: 'Typed at the cursor', copy: 'Copied to the clipboard' };

function renderHome() {
  const { stats, settings: s } = state;
  const toggle = s.trigger === 'toggle';
  $('#kpDictKeys').innerHTML = keysHtml(s.hotkey);
  $('#kpDictText').textContent = toggle
    ? 'Press to start, speak, press again. Your words appear wherever the cursor is.'
    : 'Hold, speak, let go. Your words appear wherever the cursor is.';
  $('#kpDictMeta').textContent = [OUTPUT_TEXT[s.output], s.polish ? `tidied up by ${providerName(writerId())}` : '']
    .filter(Boolean).join(' · ');
  const cmd = commandOn();
  $('#scCmd').classList.toggle('off', !cmd);
  $('#scCmdKeys').innerHTML = cmd ? keysHtml(s.command_hotkey) : '<kbd class="off">Off</kbd>';
  $('#scCmdOff').hidden = cmd;
  $('#scCmdTable').hidden = !cmd;
  $('#scCmdWho').textContent = cmd ? `Answered by ${providerName(writerId())} in a temporary chat` : '';

  $('#sSaved').textContent = fmtDuration(stats.saved_secs);
  $('#sSavedFoot').textContent = `all time, vs. typing at ${s.typing_wpm} wpm`;
  $('#sWords').textContent = fmtNum(stats.words);
  $('#sWordsToday').textContent = fmtNum(stats.words_today);
  $('#sCount').textContent = fmtNum(stats.dictations);
  $('#sWpm').textContent = fmtNum(stats.avg_wpm);
  $('#sStreak').textContent = plural(stats.streak_days, 'day');
  renderBars(stats.daily || []);

  const apps = stats.top_apps || [];
  const max = Math.max(1, ...apps.map((a) => a[1]));
  $('#topApps').innerHTML = apps.map(([name, words]) => `
    <li class="app-row"><span class="name">${esc(name)}</span><span class="n">${fmtNum(words)}</span>
    <span class="app-bar"><i style="width:${Math.max(3, (words / max) * 100)}%"></i></span></li>`).join('');
  $('#appsEmpty').hidden = apps.length > 0;

  renderHomeHead();
  renderSetup();
  loadRecent();
}

function renderBars(daily) {
  const max = Math.max(1, ...daily);
  const day = (ago) => new Date(Date.now() - ago * 86400000);
  $('#dailyBars').innerHTML = daily.map((v, i) => {
    const ago = daily.length - 1 - i;
    const label = ago === 0 ? 'Today' : day(ago).toLocaleDateString([], { weekday: 'short', month: 'short', day: 'numeric' });
    const cls = [v === 0 ? 'zero' : '', ago === 0 ? 'today' : ''].join(' ').trim();
    return `<span class="${cls}" style="height:${v ? Math.max(4, (v / max) * 100) : 0}%" title="${esc(label)}: ${plural(v, 'word')}"></span>`;
  }).join('');
  $('#dailyBars').setAttribute('aria-label', `Words per day, last ${daily.length} days: ${daily.join(', ')}`);
  const none = !daily.some((v) => v > 0);
  $('#chartMax').textContent = none ? '' : fmtNum(max);
  $('#dailyBars').classList.toggle('none', none);
  $('#chartEmpty').hidden = !none;
  $('#axisStart').textContent = day(daily.length - 1).toLocaleDateString([], { month: 'short', day: 'numeric' });
}

async function loadRecent() {
  const rows = await call('history', { query: '', kind: '', limit: 5, offset: 0 });
  $('#recentList').innerHTML = rows.map(recentHtml).join('');
  $('#recentEmpty').hidden = rows.length > 0;
}

const KIND_LABEL = { edit: 'Edit', generate: 'Write', answer: 'Ask', append: 'Continue' };

// One line per entry on Home; the full text is in History.
function recentHtml(e) {
  const kind = KIND_LABEL[e.kind];
  return `<li class="entry" data-id="${e.id}">
    <span class="r-time">${fmtTime(e.ts, true)}</span>
    <span class="r-text">${kind ? `<span class="kind">${kind}</span>` : ''}<span class="entry-text" title="${esc(e.text)}">${esc(e.text)}</span></span>
    <span class="r-side"><span class="r-app">${esc(e.app || providerName(e.provider))}</span>
      <span class="entry-actions">
        <button class="icon-btn" data-act="copy" title="Copy" aria-label="Copy">${icon('copy')}</button>
        <button class="icon-btn danger" data-act="delete" title="Delete" aria-label="Delete">${icon('trash')}</button>
      </span></span></li>`;
}

function entryHtml(e, compact) {
  const meta = [plural(e.words, 'word'), e.app, providerName(e.provider)].filter(Boolean).join(' · ');
  const kind = KIND_LABEL[e.kind];
  const note = kind
    ? `<div class="entry-note"><span class="kind">${kind}</span>${e.note ? `<q>${esc(e.note)}</q>` : ''}</div>`
    : '';
  return `<li class="entry" data-id="${e.id}">
    <div class="entry-time">${fmtTime(e.ts, compact)}</div>
    <div class="entry-body">${note}<div class="entry-text">${esc(e.text)}</div><div class="entry-meta">${esc(meta)}</div></div>
    <div class="entry-actions">
      <button class="icon-btn" data-act="copy" title="Copy" aria-label="Copy">${icon('copy')}</button>
      <button class="icon-btn danger" data-act="delete" title="Delete" aria-label="Delete">${icon('trash')}</button>
    </div></li>`;
}

document.addEventListener('click', async (ev) => {
  const btn = ev.target.closest('.entry .icon-btn');
  if (!btn) return;
  const li = btn.closest('.entry');
  if (btn.dataset.act === 'copy') {
    await call('copy_text', { text: $('.entry-text', li).textContent });
    toast('Copied to the clipboard');
  } else if (btn.dataset.act === 'delete') {
    await call('delete_entry', { id: Number(li.dataset.id) });
    if (li.closest('#historyGroups')) hist.offset = Math.max(0, hist.offset - 1);
    const group = li.closest('.day');
    li.remove();
    if (group && !$('.entry', group)) group.remove();
    if (!$('#historyGroups .entry') && $('#view-history').classList.contains('active')) loadHistory(true);
    loadRecent();
    toast('Deleted');
  }
});

// ---------- setup ----------
function renderSetup() {
  const s = state.settings;
  $('#setupCard').hidden = s.onboarded;
  if (s.onboarded) return;
  $all('.setup-provider').forEach((el) => { el.textContent = providerName(s.provider); });
  $('#setupSignIn').textContent = `Open ${providerName(s.provider)}`;
  $('#setupHotkey').innerHTML = keysHtml(s.hotkey);
  const st = status[s.provider];
  const d = describeStatus(s.provider);
  $('#stepSignin').classList.toggle('done', !!st?.ready);
  $('#setupSignIn').classList.toggle('primary', !st?.ready);
  $('#setupTry').classList.toggle('primary', !!st?.ready);
  $('#setupStatus').innerHTML = st ? `<span class="dot ${d.dot}"></span>${esc(d.text)}` : '';
}
$('#setupSignIn').addEventListener('click', () => call('provider_open', { id: state.settings.provider }));
$('#setupDone').addEventListener('click', async () => {
  await saveSettings({ onboarded: true });
  toast('You’re set. Hold the hotkey in any app to dictate');
});

// ---------- history ----------
async function loadHistory(reset) {
  if (hist.loading && !reset) return;
  hist.loading = true;
  const token = (loadHistory.token = (loadHistory.token || 0) + 1);
  const offset = reset ? 0 : hist.offset;
  let rows;
  try {
    rows = await call('history', { query: hist.query, kind: hist.kind, limit: PAGE, offset });
  } finally {
    hist.loading = false;
  }
  if (token !== loadHistory.token) return; // a newer search replaced this one
  const root = $('#historyGroups');
  if (reset) { root.innerHTML = ''; hist.offset = 0; }
  hist.offset += rows.length;
  for (const e of rows) {
    const label = dayLabel(e.ts);
    let group = root.lastElementChild;
    if (!group || group.dataset.day !== label) {
      group = document.createElement('section');
      group.className = 'day';
      group.dataset.day = label;
      group.innerHTML = `<h2 class="day-label">${esc(label)}</h2><div class="panel"><ul class="entries"></ul></div>`;
      root.appendChild(group);
    }
    $('.entries', group).insertAdjacentHTML('beforeend', entryHtml(e, false));
  }
  const empty = $('#historyEmpty');
  empty.hidden = hist.offset > 0;
  if (hist.query) empty.textContent = `Nothing matches “${hist.query}”.`;
  else if (hist.kind === 'ai') empty.textContent = commandOn()
    ? 'No commands yet. Select some text, hold the command hotkey, and say what to do with it.'
    : 'No commands yet. Turn on the command hotkey in Settings to try one.';
  else empty.textContent = state.settings.save_history
    ? 'Nothing here yet. Your dictations will show up as you go.'
    : 'History is turned off in Settings, so transcripts aren’t kept.';
  $('#loadMore').hidden = rows.length < PAGE;
}

let searchTimer;
$('#search').addEventListener('input', (e) => {
  clearTimeout(searchTimer);
  searchTimer = setTimeout(() => {
    hist.query = e.target.value.trim();
    loadHistory(true);
  }, 180);
});
$('#search').addEventListener('keydown', (e) => {
  if (e.key === 'Escape' && e.target.value) {
    e.target.value = '';
    hist.query = '';
    loadHistory(true);
  }
});
$('#historyFilter').addEventListener('click', (ev) => {
  const b = ev.target.closest('button');
  if (!b) return;
  hist.kind = b.dataset.value;
  setSeg($('#historyFilter'), hist.kind);
  loadHistory(true);
});
$('#loadMore').addEventListener('click', () => loadHistory(false));

// ---------- accounts ----------
function renderAccounts() {
  const s = state.settings;
  $('#providerSeg').innerHTML = state.providers.map((p) =>
    `<button data-value="${esc(p.id)}" role="radio">${esc(p.name)}</button>`).join('');
  setSeg($('#providerSeg'), s.provider);
  $('#writerSeg').innerHTML = state.providers.map((p) =>
    `<button data-value="${esc(p.id)}" role="radio">${esc(p.name)}</button>`).join('');
  setSeg($('#writerSeg'), writerId());
  $('#writerSeg').closest('.setting').classList.toggle('disabled', !writerInUse());
  $('#writerSeg').closest('.setting').querySelector('span').textContent = writerInUse()
    ? 'Who answers questions and rewrites text. Runs in a temporary chat, so nothing is saved to your chat history.'
    : 'Turn on the command hotkey or Polish in Settings to use this.';
  $all('.active-provider').forEach((el) => { el.textContent = providerName(s.provider); });

  $('#providerCards').innerHTML = state.providers.map((p) => {
    const roles = [];
    if (p.id === s.provider) roles.push('Dictation');
    if (writerInUse() && p.id === writerId()) roles.push('Commands');
    const d = describeStatus(p.id);
    return `<div class="account" data-id="${esc(p.id)}">
      <div class="account-top">
        <div>
          <div class="account-name">${esc(p.name)}</div>
          <div class="hint">${esc(hostOf(p.url))}</div>
        </div>
        <div class="roles">${roles.map((r) => `<span class="role">${r}</span>`).join('')}</div>
      </div>
      <div class="account-status"><span class="dot ${d.dot}"></span><span>${esc(d.text)}</span></div>
      <div class="btn-row">
        <button class="btn" data-act="signin">Open ${esc(p.name)}</button>
        ${roles.length ? '<button class="btn quiet" data-act="check">Check again</button>' : ''}
      </div>
    </div>`;
  }).join('');
}

const hostOf = (url) => { try { return new URL(url).host; } catch { return ''; } };

async function checkProvider(id) {
  if (!state || !inUse(id)) return;
  status[id] = { checking: true };
  renderStatus();
  try {
    status[id] = await invoke('provider_status', { id });
  } catch (e) {
    const msg = String(e);
    status[id] = { error: /did not respond|not ready|not loaded/i.test(msg) ? 'Page didn’t respond' : msg.slice(0, 80) };
  }
  renderStatus();
}

function checkAll() {
  const s = state.settings;
  checkProvider(s.provider);
  if (writerInUse() && writerId() !== s.provider) checkProvider(writerId());
}

function renderStatus() {
  renderChrome();
  renderSetup();
  if ($('#view-providers').classList.contains('active')) renderAccounts();
}

$('#providerCards').addEventListener('click', async (ev) => {
  const btn = ev.target.closest('button[data-act]');
  if (!btn) return;
  const id = btn.closest('.account').dataset.id;
  if (btn.dataset.act === 'signin') await call('provider_open', { id });
  else if (btn.dataset.act === 'check') checkProvider(id);
});

$('#inspectBtn').addEventListener('click', async () => {
  const out = $('#inspectOut');
  out.hidden = false;
  out.textContent = 'Inspecting…';
  try {
    out.textContent = JSON.stringify(await invoke('provider_inspect', { id: state.settings.provider }), null, 2);
  } catch (e) {
    out.textContent = String(e);
  }
});
$('#showWindowBtn').addEventListener('click', () => call('provider_show'));
$('#customizeBtn').addEventListener('click', async () => {
  const path = await call('customize_selectors');
  toast(`Opened ${path.split(/[\\/]/).pop()}. Click Reload patterns after saving`);
});
$('#reloadSelBtn').addEventListener('click', async () => {
  await call('reload_selectors');
  toast('Patterns reloaded');
});
$('#resetSelBtn').addEventListener('click', async () => {
  const ok = await confirmDialog('Reset button patterns?', 'Your edited patterns file is deleted and the built-in patterns are used again.', 'Reset');
  if (!ok) return;
  await call('reset_selectors');
  toast('Patterns reset to the defaults');
});

// ---------- settings ----------
function setSeg(seg, value) {
  $all('button', seg).forEach((b) => {
    const on = b.dataset.value === String(value);
    b.classList.toggle('on', on);
    b.setAttribute('aria-checked', on);
  });
}

function renderSettings() {
  const s = state.settings;
  if (!capturing) {
    $('#hotkeyBtn').innerHTML = keysHtml(s.hotkey);
    $('#cmdHotkeyBtn').innerHTML = s.command_hotkey ? keysHtml(s.command_hotkey) : '<span>Off</span>';
  }
  $('#cmdOffBtn').textContent = s.command_hotkey ? 'Turn off' : 'Turn on';
  $all('.seg[data-setting]').forEach((seg) => {
    if (seg.id !== 'providerSeg' && seg.id !== 'writerSeg') setSeg(seg, s[seg.dataset.setting]);
  });
  $all('input.switch[data-setting]').forEach((i) => { i.checked = !!s[i.dataset.setting]; });
  const wpm = $('input[data-setting="typing_wpm"]');
  if (document.activeElement !== wpm) wpm.value = s.typing_wpm;
  const geminiWrites = writerInUse() && writerId() === 'gemini';
  $('#modelSetting').classList.toggle('disabled', !geminiWrites);
  $('#modelHint').textContent = geminiWrites
    ? 'Which Gemini model answers commands. Your own choice in Gemini is put back afterwards.'
    : 'Only used when Gemini handles commands. ChatGPT always uses your own ChatGPT setting.';
  $('#dataDir').textContent = state.data_dir;
  $('#versionLine').textContent = `VillFlow ${state.version}`;
}

async function saveSettings(patch) {
  const next = { ...state.settings, ...patch };
  await call('save_settings', { settings: next });
  await refresh();
}

document.addEventListener('click', async (ev) => {
  const b = ev.target.closest('.seg[data-setting] button');
  if (!b || b.classList.contains('on')) return;
  const seg = b.closest('.seg');
  const key = seg.dataset.setting;
  const value = 'number' in seg.dataset ? Number(b.dataset.value) : b.dataset.value;
  await saveSettings({ [key]: value });
  if (key === 'provider' || key === 'writer') {
    toast(key === 'provider' ? `${providerName(value)} now handles dictation` : `${providerName(value)} now handles commands`);
    checkAll();
  }
});
$all('input.switch[data-setting]').forEach((i) => i.addEventListener('change', async () => {
  try {
    await saveSettings({ [i.dataset.setting]: i.checked });
    if (i.dataset.setting === 'polish' && i.checked) checkAll();
  } catch {
    i.checked = !i.checked;
  }
}));
$('input[data-setting="typing_wpm"]').addEventListener('change', (e) => {
  const v = Math.min(200, Math.max(5, Math.round(Number(e.target.value) || 40)));
  e.target.value = v;
  saveSettings({ typing_wpm: v });
});

// Hotkey capture. The global hotkeys are paused meanwhile, so pressing the
// current combo is recorded here instead of starting a dictation.
let capturing = null;
function setupHotkeyCapture(btn, setting, label) {
  const stop = () => {
    if (capturing !== btn) return;
    capturing = null;
    btn.classList.remove('capturing');
    invoke('hotkeys_pause', { paused: false }).catch(() => {});
    clearTimeout(btn.expire);
    renderSettings();
  };
  btn.addEventListener('click', () => {
    if (capturing === btn) return;
    capturing = btn;
    btn.classList.add('capturing');
    btn.textContent = 'Press the new keys';
    invoke('hotkeys_pause', { paused: true }).catch(() => {});
    clearTimeout(btn.expire);
    btn.expire = setTimeout(() => { stop(); btn.blur(); }, 18000);
  });
  btn.addEventListener('blur', stop);
  btn.addEventListener('keydown', async (e) => {
    if (capturing !== btn) return;
    e.preventDefault();
    if (e.key === 'Escape') { stop(); btn.blur(); return; }
    const mods = [];
    if (e.ctrlKey) mods.push('Ctrl');
    if (e.shiftKey) mods.push('Shift');
    if (e.altKey) mods.push('Alt');
    if (e.metaKey) mods.push('Win');
    const code = e.code;
    let key = null;
    if (/^Key[A-Z]$/.test(code)) key = code.slice(3);
    else if (/^Digit[0-9]$/.test(code)) key = code.slice(5);
    else if (/^F[0-9]{1,2}$/.test(code)) key = code;
    else if (code === 'Space') key = 'Space';
    else if (code === 'Backquote') key = '`';
    if (!key) {
      btn.innerHTML = mods.length ? `${keysHtml(mods.join('+'))}<kbd class="off">…</kbd>` : 'Press the new keys';
      return;
    }
    if (!mods.length) { toast('Hold Ctrl, Shift, Alt or Win as well', { error: true }); return; }
    const combo = [...mods, key].join('+');
    stop();
    try {
      await saveSettings({ [setting]: combo });
      toast(`${label} is now ${combo.replace(/\+/g, ' + ')}`);
    } catch { /* toast already shown */ }
    btn.blur();
  });
}
setupHotkeyCapture($('#hotkeyBtn'), 'hotkey', 'Dictation hotkey');
setupHotkeyCapture($('#cmdHotkeyBtn'), 'command_hotkey', 'Command hotkey');
$('#cmdOffBtn').addEventListener('click', async () => {
  const on = commandOn();
  const next = on ? '' : (state.settings.hotkey === 'Ctrl+Shift+X' ? 'Ctrl+Alt+X' : 'Ctrl+Shift+X');
  await saveSettings({ command_hotkey: next });
  toast(on ? 'Command hotkey turned off' : `Command hotkey is ${next.replace(/\+/g, ' + ')}`);
  if (!on) checkAll();
});

// ---------- dictionary ----------
function renderDictionary() {
  const dict = state.settings.dictionary || [];
  $('#dictList').innerHTML = dict.map((d, i) => `
    <li class="dict-row" data-i="${i}">
      <span class="from">${esc(d.from)}</span><i data-icon="arrow"></i><span class="to">${esc(d.to)}</span>
      <button class="icon-btn danger" data-act="dict-del" title="Remove" aria-label="Remove ${esc(d.from)}">${icon('trash')}</button>
    </li>`).join('');
  hydrateIcons($('#dictList'));
  $('#dictEmpty').hidden = dict.length > 0;
}

$('#dictForm').addEventListener('submit', async (ev) => {
  ev.preventDefault();
  const from = $('#dictFrom').value.trim();
  const to = $('#dictTo').value.trim();
  if (!from || !to) {
    toast('Fill in both boxes', { error: true });
    (!from ? $('#dictFrom') : $('#dictTo')).focus();
    return;
  }
  const dict = (state.settings.dictionary || []).filter((d) => d.from.toLowerCase() !== from.toLowerCase());
  const replaced = dict.length !== (state.settings.dictionary || []).length;
  await saveSettings({ dictionary: [...dict, { from, to }] });
  $('#dictFrom').value = '';
  $('#dictTo').value = '';
  $('#dictFrom').focus();
  toast(replaced ? `Updated “${from}”` : `Added “${from}” → “${to}”`);
});

$('#dictList').addEventListener('click', async (ev) => {
  const btn = ev.target.closest('[data-act="dict-del"]');
  if (!btn) return;
  const dict = [...(state.settings.dictionary || [])];
  const [removed] = dict.splice(Number(btn.closest('.dict-row').dataset.i), 1);
  await saveSettings({ dictionary: dict });
  toast(`Removed “${removed.from}”`);
});

$('#openDataBtn').addEventListener('click', () => call('open_data_dir'));
$('#clearHistoryBtn').addEventListener('click', async () => {
  const ok = await confirmDialog('Clear all history?', 'Every saved transcript is deleted from this PC. Your totals on Home are kept. This can’t be undone.', 'Clear history');
  if (!ok) return;
  await call('clear_history');
  await refresh();
  toast('History cleared');
});

// ---------- test dictation ----------
let tryOut = null; // result element waiting for the next dictation
let tryTimer = null;
let tryRun = 0; // identifies the current test, so a stale timeout can't end a newer one
const tryButtons = () => [$('#tryBtn'), $('#setupTry')];

async function runTry(out) {
  if (tryOut) return;
  tryOut = out;
  const run = ++tryRun;
  tryButtons().forEach((b) => { b.disabled = true; });
  out.hidden = false;
  out.className = 'result';
  let left = 5;
  out.textContent = `Listening. Say something (${left})`;
  tryTimer = setInterval(() => {
    left -= 1;
    out.textContent = left > 0 ? `Listening. Say something (${left})` : 'Transcribing…';
    if (left <= 0) clearInterval(tryTimer);
  }, 1000);
  try {
    await invoke('test_dictation', { seconds: 5 });
  } catch (e) {
    finishTry(String(e), true);
  }
  setTimeout(() => {
    if (tryOut === out && tryRun === run) finishTry('Nothing came back. Check the account status above and your microphone.', true);
  }, 35000);
}

function finishTry(text, isErr) {
  if (!tryOut) return;
  clearInterval(tryTimer);
  tryOut.textContent = text;
  tryOut.className = `result ${isErr ? 'err' : 'ok'}`;
  tryOut = null;
  tryButtons().forEach((b) => { b.disabled = false; });
}

$('#tryBtn').addEventListener('click', () => runTry($('#tryResult')));
$('#setupTry').addEventListener('click', () => runTry($('#setupResult')));

// ---------- live events ----------
listen('vf://dictation', (e) => {
  if (tryOut) finishTry(e.payload?.text ? `“${e.payload.text}”` : 'Heard you. History is off, so the text isn’t shown here.');
  refresh();
  if ($('#view-history').classList.contains('active')) loadHistory(true);
});
listen('vf://error', (e) => {
  if (tryOut) finishTry(String(e.payload), true);
  else toast(String(e.payload).slice(0, 160), { error: true });
});
listen('vf://empty', () => {
  if (tryOut) finishTry('No speech came through. Is the right microphone selected in Windows sound settings?', true);
});
listen('vf://settings', () => refresh());
listen('vf://goto', (e) => show(String(e.payload)));
listen('vf://signin-closed', (e) => {
  delete status[e.payload];
  if (inUse(e.payload)) setTimeout(() => checkProvider(e.payload), 2500);
  renderStatus();
});

hydrateIcons();
refresh().then(() => {
  // Opened on a specific page (e.g. "#providers" after a signed-out error).
  const view = location.hash.slice(1);
  if (view) show(view);
  checkAll();
});
