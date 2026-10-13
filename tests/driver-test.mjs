// Runs src-tauri/src/driver.js against mock ChatGPT / Gemini pages in
// headless Edge and checks the safety-critical behavior:
//   - the right dictation buttons are clicked (never voice mode),
//   - nothing that sends a message is ever clicked, whatever the config says,
//   - popups are dismissed without accepting/upgrading,
//   - the composer is read and cleared, site errors are surfaced.
//
// Usage: node tests/driver-test.mjs            (builds the debug exe if needed)
import { execFileSync } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const exe = join(root, 'src-tauri', 'target', 'debug', 'villflow.exe');
if (!existsSync(exe)) execFileSync('cargo', ['build'], { cwd: join(root, 'src-tauri'), stdio: 'inherit' });
const configs = JSON.parse(execFileSync(exe, ['--print-providers'], { encoding: 'utf8' }));
const driver = readFileSync(join(root, 'src-tauri', 'src', 'driver.js'), 'utf8');

// ---------------------------------------------------------------- the page
const page = String.raw`<!doctype html><meta charset="utf-8"><body><div id="app"></div><pre id="out">RUNNING</pre>
<script>
const CONFIGS = ${JSON.stringify(configs)};
const DRIVER_SRC = ${JSON.stringify(driver)};
const results = [];
let clicks = [];
const check = (name, cond, extra = '') => results.push((cond ? 'PASS ' : 'FAIL ') + name + (extra ? '  [' + extra + ']' : ''));

function mount(html) {
  window.__vfState = undefined;
  clicks = [];
  const app = document.getElementById('app');
  app.innerHTML = html;
  app.querySelectorAll('[contenteditable], textarea').forEach((e) => { e.style.minHeight = '24px'; e.style.minWidth = '200px'; e.style.display = 'block'; });
  app.querySelectorAll('button, [role=button]').forEach((b) => b.addEventListener('click', () => clicks.push(b.getAttribute('aria-label') || b.textContent.trim())));
}
const api = (id, patch = {}) => new Function('CFG', DRIVER_SRC)({ ...CONFIGS.find((c) => c.id === id), ...patch });
const btn = (label, extra = '') => '<button aria-label="' + label + '" ' + extra + '>x</button>';
const tbtn = (text, extra = '') => '<button ' + extra + '>' + text + '</button>';
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

(async () => {
  if (document.readyState !== 'complete') await new Promise((r) => addEventListener('load', r, { once: true }));
  // ---------- ChatGPT
  const gptIdle = '<div class="ProseMirror" contenteditable="true"><p>old draft</p></div>' + btn('Start Voice') + btn('Dictate') + btn('Send message', 'data-testid="send-button"');
  mount(gptIdle);
  let d = api('chatgpt');
  let r = d.start();
  check('chatgpt: start clicks Dictate (not voice mode)', r.ok && clicks.join() === 'Dictate', clicks.join() + ' ' + JSON.stringify(r));
  check('chatgpt: start clears the old draft', document.querySelector('.ProseMirror').innerText.trim() === '');

  const gptRec = '<div class="ProseMirror" contenteditable="true"><p></p></div>' + btn('Cancel dictation') + btn('Stop dictation') + btn('Transcribe and send');
  mount(gptRec); window.__vfState = { phase: 'recording' };
  d = api('chatgpt');
  check('chatgpt: recording detected', d.tick().recording === true);
  r = d.stop();
  check('chatgpt: stop clicks "Stop dictation"', r.via === 'stop' && clicks.join() === 'Stop dictation', clicks.join());

  // Even a hostile config can't click send.
  mount(gptRec);
  d = api('chatgpt', { stop: ['re:send', 're:transcribe', 'css:button'], cancel: ['re:send'], exclude: [] });
  d.stop(); d.cancel();
  check('guard: "Transcribe and send" never clicked, even if config asks', !clicks.some((c) => /send/i.test(c)), clicks.join());
  mount(btn('Send') + btn('Send message') + btn('Submit') + btn('Submit message') + '<div class="ProseMirror" contenteditable="true"></div>');
  d = api('chatgpt', { start: ['re:.*', 'css:button'], exclude: [] });
  d.start();
  check('guard: Send / Submit never clicked by start', clicks.length === 0, clicks.join());
  mount(btn('Sender settings') + '<div class="ProseMirror" contenteditable="true"></div>');
  d = api('chatgpt', { start: ['re:^sender'], exclude: [] });
  check('guard: only whole-word "send" is blocked', d.start().ok && clicks.join() === 'Sender settings', clicks.join());

  // Transcript lands, settles, is returned and cleared.
  mount('<div class="ProseMirror" contenteditable="true"><p></p></div>' + btn('Dictate'));
  d = api('chatgpt');
  d.start(); d.stop();
  document.querySelector('.ProseMirror').innerHTML = '<p>Hello\u00a0world.</p><p>Second line.</p>';
  d.poll(); await sleep(CONFIGS.find((c) => c.id === 'chatgpt').settle_ms + 80);
  r = d.poll();
  check('chatgpt: poll returns settled transcript', r.done && r.text === 'Hello world.\nSecond line.', JSON.stringify(r.text));
  check('chatgpt: composer cleared after reading', document.querySelector('.ProseMirror').innerText.trim() === '');

  // Signed out
  mount('<textarea id="mobile-composer-prompt"></textarea>' + tbtn('Log in') + tbtn('Sign up for free') + btn('Start dictation'));
  r = api('chatgpt').start();
  check('chatgpt: signed-out detected', !r.ok && r.error === 'signed_out' && clicks.length === 0, JSON.stringify(r));

  // ---------- Gemini
  mount('<rich-textarea><div class="ql-editor" contenteditable="true"><p><br></p></div></rich-textarea>' + btn('Upload & tools') + btn('Dictate (^⇧D)'));
  d = api('gemini');
  r = d.start();
  check('gemini: start clicks Dictate', r.ok && clicks.join() === 'Dictate (^⇧D)', clicks.join());
  mount('<rich-textarea><div class="ql-editor" contenteditable="true"><p>partial</p></div></rich-textarea>' + btn('Stop dictation (^⇧D)'));
  window.__vfState = { phase: 'recording' };
  r = api('gemini').stop();
  check('gemini: stop clicks Stop dictation', r.via === 'stop' && clicks.join() === 'Stop dictation (^⇧D)', clicks.join());
  mount('<rich-textarea><div class="ql-editor" contenteditable="true"><p></p></div></rich-textarea>');
  let shortcut = null;
  document.addEventListener('keydown', (e) => { if (e.ctrlKey && e.shiftKey && e.code === 'KeyD') shortcut = 'ctrl+shift+D'; }, { once: true });
  r = api('gemini').start();
  check('gemini: falls back to Ctrl+Shift+D when the button is missing', r.ok && r.button === 'shortcut' && shortcut === 'ctrl+shift+D', JSON.stringify(r));

  // ---------- send guard: other sites' submit buttons
  mount('<div class="ProseMirror" contenteditable="true"><p>text</p></div>' + btn('Submit', 'data-testid="chat-submit"') + tbtn('Submit'));
  window.__vfState = { phase: 'recording' };
  d = api('chatgpt', { stop: ['re:^submit$', "css:[data-testid='chat-submit']"], toggle_stop: false });
  r = d.stop();
  check('guard: a config pointing at Submit still clicks nothing', clicks.length === 0 && r.via === 'none', clicks.join());

  // ---------- popups
  mount('<div class="ProseMirror" contenteditable="true"></div>' + btn('Dictate')
    + '<div id="onetrust">' + tbtn('Accept All Cookies') + tbtn('Reject All') + '</div>'
    + '<div role="dialog" aria-modal="true">' + tbtn('Upgrade to Pro') + tbtn('Maybe later') + '</div>');
  r = api('chatgpt').status();
  check('popups: rejects cookies, closes upsell with "Maybe later"', clicks.includes('Reject All') && clicks.includes('Maybe later'), clicks.join());
  check('popups: never accepts or upgrades', !clicks.some((c) => /accept|upgrade/i.test(c)), clicks.join());
  mount('<div role="dialog" aria-modal="true">' + tbtn('Continue') + tbtn('Pro plan') + tbtn('Log in') + '</div>');
  let esc = false; document.addEventListener('keydown', (e) => { if (e.key === 'Escape') esc = true; }, { once: true });
  api('chatgpt').status();
  check('popups: unknown modal gets Escape, no risky clicks', esc && clicks.length === 0, clicks.join());
  mount('<div class="ProseMirror" contenteditable="true"></div>' + btn('Hide upsell banner') + tbtn('Upgrade'));
  api('chatgpt', { dismiss: ['re:^hide upsell banner$'] }).status();
  check('popups: a provider-specific dismiss rule works, never "Upgrade"', clicks.join() === 'Hide upsell banner', clicks.join());

  // ---------- site errors
  mount('<div class="ProseMirror" contenteditable="true"></div>' + btn('Dictate') + '<div role="alert">Dictation failed. Please try again.</div>');
  d = api('chatgpt'); d.start(); d.stop();
  await sleep(CONFIGS.find((c) => c.id === 'chatgpt').empty_timeout_ms + 50);
  r = d.poll();
  check('errors: site error surfaced when no text', r.done && r.text === '' && /Dictation failed/.test(r.page_error), JSON.stringify(r));

  // ---------- cancel after quick tap
  mount('<div class="ProseMirror" contenteditable="true"></div>' + btn('Cancel dictation') + btn('Stop dictation'));
  r = api('chatgpt').cancel();
  check('cancel: clicks Cancel dictation and reports it was recording', r.was_recording && clicks.join() === 'Cancel dictation', clicks.join());

  // ---------- ask (command mode / polish), Gemini
  const gemAsk = (extra = '') => '<rich-textarea><div class="ql-editor" contenteditable="true"><p><br></p></div></rich-textarea>'
    + btn('New chat') + btn('Temporary chat') + btn('Open mode picker, currently Pro') + btn('Dictate (^⇧D)') + btn('Send message') + extra;
  mount(gemAsk());
  d = api('gemini');
  r = d.askTemporary();
  check('ask: turns temporary chat on when off', r.clicked && clicks.join() === 'Temporary chat', clicks.join());
  mount(gemAsk('<h1>Temporary chats</h1>'));
  r = api('gemini').askTemporary();
  check('ask: never toggles temporary chat off', r.temporary === true && clicks.length === 0, clicks.join());

  mount(gemAsk('<div role="menu"><div role="menuitem">3.5 Flash-Lite Fastest answers</div><div role="menuitem">3.8 Flash All-around help</div><div role="menuitem">3.1 Pro Advanced reasoning</div></div>'));
  document.querySelectorAll('[role=menuitem]').forEach((m) => m.addEventListener('click', () => clicks.push(m.textContent)));
  d = api('gemini');
  const gcfg = CONFIGS.find((c) => c.id === 'gemini').ask;
  r = d.askModeOpen(gcfg.name_balanced);
  check('ask: opens the model menu when not on Flash', r.opened && r.current === 'Pro', JSON.stringify(r));
  r = d.askModePick(gcfg.model_balanced);
  check('ask: picks 3.8 Flash, not Flash-Lite', r.ok && clicks.includes('3.8 Flash All-around help') && !clicks.some((c) => /Lite/.test(c)), clicks.join());

  mount(gemAsk());
  r = api('gemini').askSend('Rewrite formally: hey thx');
  check('ask: Gemini refuses to send outside temporary chat', !r.ok && /temporary/.test(r.error) && !clicks.includes('Send message'), JSON.stringify(r));
  mount(gemAsk('<h1>Temporary chats</h1>'));
  d = api('gemini');
  r = d.askSend('Rewrite formally: hey thx');
  check('ask: types the prompt, then clicks Send', r.ok && clicks.join() === 'Send message'
    && document.querySelector('.ql-editor').innerText.trim() === 'Rewrite formally: hey thx', JSON.stringify(r) + ' ' + clicks.join());
  r = d.askPoll();
  check('ask: not done before an answer exists', r.done === false, JSON.stringify(r));
  document.getElementById('app').insertAdjacentHTML('beforeend',
    '<model-response><message-content><p>Hi team,</p><p>Thank you for finishing early.</p></message-content></model-response>');
  r = d.askPoll(); await sleep(gcfg.settle_ms + 100); r = d.askPoll();
  check('ask: a rendered answer without a code block gets extra time to settle', r.done === false, JSON.stringify(r));
  await sleep(2500 - gcfg.settle_ms); r = d.askPoll();
  check('ask: answer read with its paragraph break', r.done && r.text === 'Hi team,\n\nThank you for finishing early.', JSON.stringify(r.text));
  mount(gemAsk(btn('Stop response') + '<h1>Temporary chats</h1>'));
  d = api('gemini'); d.askSend('x');
  document.getElementById('app').insertAdjacentHTML('beforeend', '<model-response><message-content><p>partial</p></message-content></model-response>');
  d.askPoll(); await sleep(gcfg.settle_ms + 100); r = d.askPoll();
  check('ask: waits while the answer is still being written', r.done === false && r.generating === true, JSON.stringify(r));
  // Dictation methods still never send, even on the ask-enabled page.
  mount(gemAsk());
  d = api('gemini'); d.start(); d.stop(); d.cancel();
  check('guard: dictation on Gemini never clicks Send', !clicks.includes('Send message'), clicks.join());

  // ---------- ask on ChatGPT: temporary chat required, code block read verbatim
  const gptAsk = (extra = '') => '<div class="ProseMirror" contenteditable="true"><p><br></p></div>'
    + btn('Temporary chat') + btn('Dictate') + btn('Send') + extra;
  mount(gptAsk());
  r = api('chatgpt').askSend('Answer briefly: 2+2');
  check('ask: ChatGPT refuses to send outside temporary chat', !r.ok && /temporary/.test(r.error) && !clicks.includes('Send'), JSON.stringify(r) + ' ' + clicks.join());
  mount(gptAsk(btn('Turn off temporary chat')));
  d = api('chatgpt');
  r = d.askState();
  check('ask: ChatGPT temporary chat detected', r.temporary === true && r.fresh === true, JSON.stringify(r));
  r = d.askSend('Answer briefly: 2+2');
  check('ask: ChatGPT sends in temporary chat', r.ok && clicks.includes('Send'), JSON.stringify(r));
  const gcode = 'ANSWER\n- first line\n- second line\n\nLast paragraph with **stars** kept as typed.';
  document.getElementById('app').insertAdjacentHTML('beforeend',
    '<div data-content-search-unit-key="fallback-turn-0:2:assistant"><p>Plain text</p><div data-markdown-copy="code-block"><div><code></code></div></div></div>');
  document.querySelector('[data-markdown-copy] code').textContent = gcode;
  d.askPoll(); await sleep(CONFIGS.find((c) => c.id === 'chatgpt').ask.settle_ms + 100); r = d.askPoll();
  check('ask: ChatGPT answer read from the code block, verbatim', r.done && r.format === 'code' && r.text === gcode, JSON.stringify(r));

  mount(gptAsk(btn('Turn off temporary chat')));
  d = api('chatgpt'); d.askSend('Draw a cat');
  document.getElementById('app').insertAdjacentHTML('beforeend', '<div data-content-search-unit-key="x:1:assistant"><img src="data:image/gif;base64,R0lGODlhAQABAAAAACw=" width="40" height="40"></div>');
  d.askPoll(); await sleep(4300); r = d.askPoll();
  check('ask: an image-only answer fails fast with a clear reason', r.done && r.text === '' && /image/.test(r.page_error || ''), JSON.stringify(r));

  // ChatGPT's answer bubble starts with only a screen-reader label, and its stop button blinks.
  mount(gptAsk(btn('Turn off temporary chat')));
  d = api('chatgpt'); d.askSend('Fix this');
  document.getElementById('app').insertAdjacentHTML('beforeend', '<div data-content-search-unit-key="y:1:assistant"><h4 class="sr-only">ChatGPT said:</h4></div>');
  d.askPoll(); await sleep(3000); r = d.askPoll();
  check('ask: a "ChatGPT said:" label alone is not an answer', !r.done, JSON.stringify(r));
  document.querySelector('[data-content-search-unit-key="y:1:assistant"]').insertAdjacentHTML('beforeend', '<div data-markdown-copy="code-block"><pre><code>REPLACE\nFixed.</code></pre></div>');
  d.askPoll(); await sleep(CONFIGS.find((c) => c.id === 'chatgpt').ask.settle_ms + 100); r = d.askPoll();
  check('ask: then the real answer is read, without the label', r.done && r.format === 'code' && r.text === 'REPLACE\nFixed.', JSON.stringify(r));

  // Gemini's code block element
  mount(gemAsk('<h1>Temporary chats</h1>'));
  d = api('gemini'); d.askSend('x');
  document.getElementById('app').insertAdjacentHTML('beforeend',
    '<model-response><message-content><code-block><div class="header">Text</div><pre><code>REPLACE\nHello there.</code></pre></code-block></message-content></model-response>');
  d.askPoll(); await sleep(gcfg.settle_ms + 100); r = d.askPoll();
  check('ask: Gemini answer read from its code block', r.done && r.format === 'code' && r.text === 'REPLACE\nHello there.', JSON.stringify(r));

  const failed = results.filter((l) => l.startsWith('FAIL')).length;
  document.getElementById('out').textContent = results.join('\n') + '\n' + (failed ? failed + ' FAILED' : 'ALL ' + results.length + ' PASSED');
})().catch((e) => { document.getElementById('out').textContent = 'CRASH ' + e.stack; });
</script>`;

const outDir = join(root, 'tests', 'out');
mkdirSync(outDir, { recursive: true });
const html = join(outDir, 'driver-test.html');
writeFileSync(html, page);

const edge = ['C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe', 'C:/Program Files/Microsoft/Edge/Application/msedge.exe'].find(existsSync);
// Own throwaway profile: otherwise, if Edge is already open, the launch is handed
// to that browser and exits without output.
const profile = join(outDir, 'edge-profile');
const dom = execFileSync(edge, ['--headless=new', '--disable-gpu', `--user-data-dir=${profile}`, '--no-first-run',
  '--virtual-time-budget=30000', '--dump-dom', pathToFileURL(html).href], { encoding: 'utf8' });
const out = (dom.match(/<pre id="out">([\s\S]*?)<\/pre>/) || [])[1] || dom.slice(0, 2000);
const text = out.replace(/&lt;/g, '<').replace(/&gt;/g, '>').replace(/&amp;/g, '&').replace(/&quot;/g, '"');
console.log(text);
if (!text.trim()) {
  console.log('Headless Edge returned nothing. Open tests/out/driver-test.html through a local web server');
  console.log('(e.g. python -m http.server in tests/out) and read the results on the page.');
}
process.exit(/FAILED|CRASH|RUNNING/.test(text) || !/PASSED/.test(text) ? 1 : 0);
