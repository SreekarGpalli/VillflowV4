// VillFlow provider driver — the body of a function evaluated inside the
// provider page (gemini.google.com / chatgpt.com). `CFG` (the
// provider's selector config) is in scope; this must `return` the API object.
//
// It only ever clicks the site's own dictation buttons and reads/clears the
// composer. It must never send a chat message: see `NEVER_CLICK`.

const S = (window.__vfState ||= {
  phase: 'idle', sawRecording: false, lastText: '', lastChange: 0,
  stopAt: 0, restarts: 0, lastRestart: 0, carry: '',
});

// Hard guard: anything that would send a chat message ("Send", "Send message",
// "Transcribe and send", "Submit"…). Never click these, whatever the config says.
const NEVER_CLICK = /\bsend\b|^submit( (message|prompt|query))?$/i;
const NEVER_CLICK_CSS = "[data-testid='send-button'],[data-testid='chat-submit'],button.send-button,[aria-label='Send message']";

const visible = (el) => {
  if (!el || !el.isConnected) return false;
  const r = el.getBoundingClientRect();
  if (r.width < 2 || r.height < 2) return false;
  const cs = getComputedStyle(el);
  return cs.visibility !== 'hidden' && cs.display !== 'none' && cs.opacity !== '0';
};

const attrs = (el) => [
  el.getAttribute('aria-label'), el.getAttribute('title'), el.getAttribute('data-testid'),
  el.getAttribute('data-test-id'), el.getAttribute('mattooltip'), (el.innerText || '').trim().slice(0, 40),
].filter(Boolean);

const describe = (el) => attrs(el).join(' | ');
const regex = (p) => new RegExp(p.replace(/^re:/, ''), 'i');
const forbidden = (el) => el.matches(NEVER_CLICK_CSS) || attrs(el).some((a) => NEVER_CLICK.test(a.trim()));

const excluded = (el) => forbidden(el) || (CFG.exclude || []).some((p) =>
  p.startsWith('css:') ? el.matches(p.slice(4)) : attrs(el).some((a) => regex(p).test(a)));

const buttons = () => [...document.querySelectorAll('button, [role="button"]')].filter(visible);

function find(patterns) {
  let btns = null;
  for (const p of patterns || []) {
    if (p.startsWith('css:')) {
      const el = [...document.querySelectorAll(p.slice(4))].find((e) => visible(e) && !forbidden(e));
      if (el) return el;
      continue;
    }
    btns ||= buttons();
    const re = regex(p);
    const el = btns.find((b) => !excluded(b) && attrs(b).some((a) => re.test(a)));
    if (el) return el;
  }
  return null;
}

function click(el) {
  if (!el || forbidden(el)) return false;
  el.click();
  return true;
}

function composer() {
  for (const sel of CFG.composer || []) {
    const el = [...document.querySelectorAll(sel)].find(visible);
    if (el) return el;
  }
  return null;
}

const clean = (t) => (t || '')
  .replace(/[\u00a0\u202f]/g, ' ').replace(/\u200b/g, '')
  // Editors render each paragraph as a block, which innerText turns into a blank
  // line; a dictated line break should stay a single newline.
  .replace(/[ \t]+\n/g, '\n').replace(/\n{2,}/g, '\n').trim();

const isField = (el) => el && (el.tagName === 'TEXTAREA' || el.tagName === 'INPUT');
const readText = (el) => (el ? clean(isField(el) ? el.value : el.innerText) : '');

function clearComposer(el) {
  if (!el || !readText(el)) return;
  if (isField(el)) {
    const proto = el.tagName === 'TEXTAREA' ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(proto, 'value').set.call(el, '');
    el.dispatchEvent(new Event('input', { bubbles: true }));
    return;
  }
  // Rich editors (ProseMirror / Quill / TipTap) track their own state, so delete
  // through the editing pipeline instead of wiping innerHTML.
  el.focus({ preventScroll: true });
  const range = document.createRange();
  range.selectNodeContents(el);
  const sel = getSelection();
  sel.removeAllRanges();
  sel.addRange(range);
  document.execCommand('delete');
  if (readText(el)) {
    el.innerHTML = '<p><br></p>';
    el.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'deleteContentBackward' }));
  }
}

function isRecording() {
  if (find(CFG.recording)) return true;
  const b = find(CFG.start);
  return !!b && (b.getAttribute('aria-pressed') === 'true' || /\b(recording|listening)\b/i.test(String(b.className)));
}

// ---- popups -------------------------------------------------------------
// Cookie banners, "what's new", upgrade nags, etc. We only ever click buttons
// that decline/close; never anything that logs in, upgrades or accepts.
const DISMISS = [
  're:^reject( all| non-essential| optional)?( cookies)?$', 're:^(only|use) (necessary|essential)( cookies)?$',
  're:^decline( all)?$', 're:^dismiss( cookie notice)?', 're:^(maybe later|not now|no thanks|skip|got it|okay|ok)$',
  're:^stay logged out$', 're:^close( dialog| modal| banner)?$',
];
const NEVER_DISMISS = /log ?in|sign ?(in|up)|upgrade|subscribe|accept|allow|try |start|continue|clear|delete|buy|plus|\bpro\b/i;

function dismissPopups() {
  const closed = [];
  const dialogs = [...document.querySelectorAll('[role="dialog"], [role="alertdialog"], dialog[open], [aria-modal="true"]')]
    .filter(visible);
  // Cookie banners often aren't dialogs, so also scan the whole page for reject buttons.
  const scopes = [...dialogs, document];
  for (const scope of scopes) {
    const btns = [...scope.querySelectorAll('button, [role="button"]')].filter(visible);
    const custom = CFG.dismiss || [];
    for (const p of [...custom, ...DISMISS]) {
      const re = regex(p);
      const isCustom = custom.includes(p);
      const el = btns.find((b) => !forbidden(b) && attrs(b).some((a) => re.test(a.trim()))
        && (isCustom || !attrs(b).some((a) => NEVER_DISMISS.test(a) && !/^dismiss/i.test(a))));
      // Page-wide (outside dialogs) only cookie rejections and provider-specific rules.
      if (el && (scope !== document || isCustom || /reject|necessary|essential|decline|dismiss cookie/i.test(describe(el)))) {
        closed.push(describe(el));
        el.click();
        break;
      }
    }
  }
  // A modal with no recognizable close button: Escape closes most of them.
  if (!closed.length && dialogs.some((d) => d.getAttribute('aria-modal') === 'true' || d.getAttribute('role') === 'dialog')) {
    document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', code: 'Escape', keyCode: 27, bubbles: true }));
    closed.push('Escape');
  }
  return closed;
}

// Keyboard-shortcut fallback (e.g. Gemini: Ctrl+Shift+D) when the button isn't found.
function pressShortcut(sc) {
  if (!sc || !sc.key) return false;
  const target = composer() || document.body;
  const init = {
    key: sc.key, code: sc.code || `Key${sc.key.toUpperCase()}`, bubbles: true, cancelable: true,
    ctrlKey: !!sc.ctrl, shiftKey: !!sc.shift, altKey: !!sc.alt, metaKey: !!sc.meta,
  };
  target.dispatchEvent(new KeyboardEvent('keydown', init));
  target.dispatchEvent(new KeyboardEvent('keyup', init));
  return true;
}

// The site's own error toasts/alerts ("Dictation failed", "limit reached",
// "microphone blocked"…), so we can show the real reason instead of "no speech".
function pageError() {
  const els = [...document.querySelectorAll('[role="alert"], [role="status"], [data-sonner-toast], .toast, mat-snack-bar-container, simple-snack-bar')]
    .filter(visible);
  for (const el of els) {
    const t = clean(el.innerText);
    if (t && t.length < 200 && /error|fail|couldn.t|could not|unable|limit|try again|blocked|denied|permission|microphone|not available/i.test(t)
        && !/is active and in use/i.test(t)) {
      return t;
    }
  }
  return '';
}

const busy = () => !!find(CFG.busy);
const signedOut = () => !!find(CFG.signed_out);

function status() {
  const popups = dismissPopups();
  const mic = !!find(CFG.start) || isRecording();
  const box = !!composer();
  const out = signedOut();
  return {
    ok: true, url: location.href, title: document.title, loading: document.readyState === 'loading',
    signed_out: out, mic_button: mic, composer: box, ready: mic && box && !out, recording: isRecording(), popups,
  };
}

function start() {
  if (document.readyState === 'loading') return { ok: false, error: 'loading', retry: true };
  const popups = dismissPopups();
  if (signedOut()) return { ok: false, error: 'signed_out' };
  const c = composer();
  if (!c) return { ok: false, error: 'no_composer', retry: true };
  // A dictation left running from an earlier attempt: discard it first.
  if (isRecording()) click(find(CFG.cancel));
  const b = find(CFG.start);
  if (!b && !CFG.start_shortcut) return { ok: false, error: 'no_mic_button', retry: true, popups };
  // Popups we just closed may still be animating out; let the caller retry.
  if (popups.length && !b) return { ok: false, error: 'popup', retry: true, popups };
  clearComposer(c);
  Object.assign(S, {
    phase: 'recording', sawRecording: false, restarts: 0, lastRestart: Date.now(),
    stopAt: 0, lastText: '', lastChange: Date.now(), carry: '',
  });
  if (b) click(b);
  else pressShortcut(CFG.start_shortcut);
  return { ok: true, button: b ? describe(b) : 'shortcut', popups };
}

// Called ~4x/sec while the user holds the hotkey.
function tick() {
  const text = readText(composer());
  const rec = isRecording();
  if (rec) S.sawRecording = true;
  // Some providers (Gemini) stop listening after a pause. Only restart when we
  // positively saw recording and then saw it end, so a wrong selector can't
  // cause an on/off click loop.
  if (S.phase === 'recording' && CFG.restart_on_silence && S.sawRecording && !rec && !busy()
      && Date.now() - S.lastRestart > 1200) {
    const b = find(CFG.start);
    if (b) {
      S.carry = text;
      click(b);
      S.restarts += 1;
      S.lastRestart = Date.now();
      S.sawRecording = false;
    }
  }
  return {
    ok: true, recording: rec, saw_recording: S.sawRecording, preview: text, restarts: S.restarts,
    page_error: rec ? '' : pageError(),
  };
}

function stop() {
  if (isRecording()) S.sawRecording = true;
  Object.assign(S, { phase: 'stopping', stopAt: Date.now(), lastText: readText(composer()), lastChange: Date.now() });
  const s = find(CFG.stop);
  if (s && click(s)) return { ok: true, via: 'stop', button: describe(s) };
  if (CFG.toggle_stop && isRecording()) {
    const b = find(CFG.start);
    if (click(b)) return { ok: true, via: 'toggle', button: describe(b) };
  }
  return { ok: true, via: 'none' };
}

// Polled after stop() until the transcript has landed in the composer.
function poll() {
  const c = composer();
  const now = Date.now();
  let text = readText(c);
  if (text !== S.lastText) {
    S.lastText = text;
    S.lastChange = now;
  }
  const rec = isRecording();
  const bz = busy();
  const elapsed = now - S.stopAt;
  const quiet = now - S.lastChange;
  let reason = '';
  if (!rec && !bz && text && quiet >= CFG.settle_ms) reason = 'settled';
  else if (!rec && !bz && !text && elapsed >= CFG.empty_timeout_ms) reason = 'empty';
  else if (elapsed >= CFG.timeout_ms) reason = 'timeout';
  if (!reason) return { ok: true, done: false, recording: rec, busy: bz, preview: text };

  if (S.carry && !text.startsWith(S.carry)) text = `${S.carry} ${text}`.trim();
  // Never leave the provider recording after we're done with it.
  if (rec) click(find(CFG.cancel)) || click(find(CFG.stop));
  clearComposer(c);
  S.phase = 'idle';
  S.carry = '';
  return { ok: true, done: true, text, reason, page_error: text ? '' : pageError() };
}

function cancel() {
  const wasRecording = isRecording();
  if (!click(find(CFG.cancel)) && wasRecording) {
    click(find(CFG.stop) || (CFG.toggle_stop ? find(CFG.start) : null));
  }
  S.phase = 'idle';
  S.carry = '';
  // The provider may still drop a transcript in after cancel; wipe it shortly after.
  setTimeout(() => { if (S.phase === 'idle') clearComposer(composer()); }, 1500);
  return { ok: true, was_recording: wasRecording };
}

// ---- ask (command mode / polish) -------------------------------------------
// The ONLY place a message is ever sent, and only the prompt VillFlow itself
// just typed into the box (checked right before the click). Runs in the site's
// temporary-chat mode so nothing lands in the user's chat history.
const A = CFG.ask || {};

// Like find(), but ignores the send guard: used for the send/stop buttons here.
function findRaw(patterns) {
  let btns = null;
  for (const p of patterns || []) {
    if (p.startsWith('css:')) {
      const el = [...document.querySelectorAll(p.slice(4))].find(visible);
      if (el) return el;
      continue;
    }
    btns ||= buttons();
    const re = regex(p);
    const el = btns.find((b) => attrs(b).some((a) => re.test(a)));
    if (el) return el;
  }
  return null;
}

const menuItems = () => [...document.querySelectorAll(
  '[role="menuitem"], [role="menuitemradio"], [role="option"], .mat-mdc-menu-item',
)].filter(visible);

function currentMode() {
  const picker = find(A.mode_picker);
  if (!picker || !A.mode_current) return '';
  const m = (picker.getAttribute('aria-label') || '').match(new RegExp(A.mode_current, 'i'));
  return m ? m[1].trim() : '';
}

const temporaryOn = () => (!!(A.temporary_on || []).length && !!findRaw(A.temporary_on))
  || (!!A.temporary_active && new RegExp(A.temporary_active, 'i').test(document.body.innerText));
const needsTemporary = () => !!(A.temporary || []).length;

function askNewChat() {
  return { ok: true, clicked: click(find(A.new_chat)) };
}

function askTemporary() {
  if (!(A.temporary || []).length) return { ok: true, temporary: false };
  if (temporaryOn()) return { ok: true, temporary: true };
  const b = find(A.temporary);
  if (!b) return { ok: false, error: 'no temporary chat button' };
  click(b);
  return { ok: true, clicked: true };
}

// Open the model menu if the current model doesn't match `nameRe`.
function askModeOpen(nameRe) {
  const now = currentMode();
  if (!nameRe || !now) return { ok: true, current: now, opened: false };
  if (new RegExp(nameRe, 'i').test(now)) return { ok: true, current: now, opened: false };
  return { ok: true, current: now, opened: click(find(A.mode_picker)) };
}

function askModePick(optionRe) {
  const re = new RegExp(optionRe, 'i');
  const item = menuItems().find((e) => re.test(clean(e.innerText).replace(/\n/g, ' ')));
  if (!item) {
    document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', code: 'Escape', keyCode: 27, bubbles: true }));
    return { ok: false, error: 'model not in menu' };
  }
  item.click();
  return { ok: true, current: currentMode() };
}

function askSend(prompt) {
  const c = composer();
  if (!c) return { ok: false, error: 'no_composer', retry: true };
  if (isRecording()) click(find(CFG.cancel));
  clearComposer(c);
  c.focus({ preventScroll: true });
  if (!document.execCommand('insertText', false, prompt)) {
    if (isField(c)) {
      const proto = c.tagName === 'TEXTAREA' ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
      Object.getOwnPropertyDescriptor(proto, 'value').set.call(c, prompt);
      c.dispatchEvent(new Event('input', { bubbles: true }));
    }
  }
  // Never send outside temporary mode: the chat would land in the user's history.
  if (needsTemporary() && !temporaryOn()) return { ok: false, error: 'temporary chat is not on', retry: true };
  // Safety: only ever send exactly what we typed.
  const want = clean(prompt).replace(/\s+/g, ' ');
  const got = readText(c).replace(/\s+/g, ' ');
  if (got !== want) return { ok: false, error: 'prompt did not land in the message box', retry: true };
  const send = findRaw(A.send);
  if (!send || send.disabled || send.getAttribute('aria-disabled') === 'true') {
    return { ok: false, error: 'send button not ready', retry: true };
  }
  S.askCount = [...document.querySelectorAll(A.response)].length;
  S.askStart = Date.now();
  S.askGenAt = 0;
  S.askLast = '';
  S.askChange = Date.now();
  send.click();
  return { ok: true };
}

// Written answers keep their paragraph breaks (one blank line between them),
// unlike dictation, where every editor block is just a line break.
// Screen-reader-only labels inside an answer ("ChatGPT said:") aren't part of it.
const SR_ONLY = '.sr-only, .visually-hidden, .cdk-visually-hidden';
const visibleText = (el) => {
  let t = el.innerText || '';
  for (const h of el.querySelectorAll(SR_ONLY)) {
    const label = (h.textContent || '').trim();
    if (label) t = t.replace(label, '');
  }
  return t;
};
const readAnswer = (el) => visibleText(el)
  .replace(/[\u00a0\u202f]/g, ' ').replace(/\u200b/g, '')
  .replace(/[ \t]+\n/g, '\n').replace(/\n{3,}/g, '\n\n').trim();

// The answer's text: its first code block when there is one (shown verbatim by
// the chat app, so lists/line breaks survive exactly), else the rendered text.
function answerOf(el) {
  for (const sel of (A.code_block || '').split(',').map((x) => x.trim()).filter(Boolean)) {
    const code = el.querySelector(sel);
    if (code) return { text: code.textContent.replace(/\r\n/g, '\n').trim(), format: 'code' };
  }
  return { text: readAnswer(el), format: 'plain' };
}

function askPoll() {
  const rs = [...document.querySelectorAll(A.response)];
  const now = Date.now();
  // The stop button re-renders now and then; a blink doesn't mean the answer is done.
  if (findRaw(A.generating)) S.askGenAt = now;
  const generating = now - (S.askGenAt || 0) < 1500;
  const last = rs.length > S.askCount ? rs[rs.length - 1] : null;
  const ans = last ? answerOf(last) : { text: '', format: '' };
  const text = ans.text;
  if (text !== S.askLast) {
    S.askLast = text;
    S.askChange = now;
  }
  const elapsed = now - S.askStart;
  // We ask for a code block; rendered text without one may still turn into one.
  const settle = ans.format === 'code' ? (A.settle_ms || 700) : Math.max(A.settle_ms || 700, 2500);
  if (text && !generating && now - S.askChange >= settle) return { ok: true, done: true, text, format: ans.format };
  if (elapsed >= (A.timeout_ms || 60000)) return { ok: true, done: true, text, format: ans.format, reason: 'timeout' };
  const err = !text && !generating && elapsed > 4000 ? pageError() : '';
  if (err) return { ok: true, done: true, text: '', page_error: err };
  // An answer bubble that's finished but has no text: an image or other media.
  if (last && !text && !generating) {
    const quiet = now - S.askChange;
    if (quiet > 4000 && last.querySelector('img, canvas, video')) {
      return { ok: true, done: true, text: '', page_error: 'the answer was an image, and VillFlow can only paste text' };
    }
    if (quiet > 12000) return { ok: true, done: true, text: '', page_error: 'the answer had no text' };
  }
  return { ok: true, done: false, generating, chars: text.length };
}

// Diagnostics: what the driver can see, for fixing selectors after a site redesign.
function inspect() {
  const tag = (e) => e.tagName.toLowerCase() + (e.id ? `#${e.id}` : '')
    + (typeof e.className === 'string' && e.className.trim()
      ? `.${e.className.trim().split(/\s+/).slice(0, 3).join('.')}` : '');
  return {
    ok: true,
    status: status(),
    buttons: buttons().map(describe).filter(Boolean).slice(0, 120),
    editors: [...document.querySelectorAll('textarea, [contenteditable="true"]')].filter(visible).map(tag),
  };
}

return {
  status, start, tick, stop, poll, cancel, inspect,
  dismiss: () => ({ ok: true, closed: dismissPopups() }),
  askNewChat, askTemporary, askModeOpen, askModePick, askSend, askPoll,
  askState: () => ({
    ok: true, temporary: temporaryOn(), url: location.href, composer: !!composer(),
    // A brand-new chat: nothing asked in it yet, so it can be used as is.
    fresh: !!A.response && document.querySelectorAll(A.response).length === 0,
  }),
};
