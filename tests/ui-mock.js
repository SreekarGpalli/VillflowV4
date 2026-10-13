// Stand-in for the Tauri backend, so ui/ can be previewed in a normal browser
// (tests/ui-preview.mjs copies ui/ into tests/out/ui with this loaded first).
// URL options: ?fresh (first run), ?signedout, ?empty, ?hotkeyerror, ?cmdoff, ?click=<selector>, ?css=<rules>, ?error=<text>
(() => {
  const q = new URLSearchParams(location.search);
  const now = Date.now();
  const H = 3600000;
  const settings = {
    provider: 'gemini', hotkey: 'Ctrl+Shift+Space', command_hotkey: q.has('cmdoff') ? '' : 'Ctrl+Shift+X',
    trigger: 'hold', output: 'paste', restore_clipboard: true, add_trailing_space: true,
    launch_at_login: true, save_history: true, typing_wpm: 40, free_memory_after_mins: 0,
    polish: false, writer: 'chatgpt', writing_model: 'balanced',
    dictionary: q.has('empty') ? [] : [{ from: 'villa flow', to: 'VillFlow' }, { from: 'sreekar', to: 'Sreekar' }, { from: 'tory', to: 'Tauri' }],
    onboarded: !q.has('fresh'),
  };
  let entries = q.has('empty') ? [] : [
    { id: 9, ts: now - 0.2 * H, text: 'Can you push the release notes to the shared drive before the 4 pm call? I want Priya to review the pricing section.', words: 22, duration_ms: 7400, provider: 'gemini', app: 'Slack', kind: 'dictation', note: '' },
    { id: 8, ts: now - 0.6 * H, text: 'The migration finished at 2:14 and every table checked out. I’ll keep the old cluster running until Friday in case we need to roll back.', words: 27, duration_ms: 9100, provider: 'chatgpt', app: 'Outlook', kind: 'edit', note: 'make this sound more confident' },
    { id: 7, ts: now - 1.4 * H, text: 'Canberra', words: 1, duration_ms: 2100, provider: 'chatgpt', app: 'Notepad', kind: 'generate', note: 'what’s the capital of Australia' },
    { id: 6, ts: now - 3 * H, text: 'The main point: the vendor can deliver in six weeks only if we sign by the 20th, otherwise it slips to March.', words: 22, duration_ms: 3000, provider: 'chatgpt', app: 'Chrome', kind: 'answer', note: 'what’s the main point of this' },
    { id: 5, ts: now - 26 * H, text: 'Remind me to renew the domain and move the DNS records over to the new registrar.', words: 16, duration_ms: 5200, provider: 'gemini', app: 'Notion', kind: 'dictation', note: '' },
    { id: 4, ts: now - 27 * H, text: 'Thanks for the quick turnaround. The second draft reads much better, and the numbers in table two now match the appendix.', words: 22, duration_ms: 7800, provider: 'gemini', app: 'Outlook', kind: 'dictation', note: '' },
    { id: 3, ts: now - 75 * H, text: 'fix the flaky upload test by waiting for the progress event instead of sleeping', words: 14, duration_ms: 4800, provider: 'grok', app: 'Code', kind: 'dictation', note: '' },
  ];
  const daily = q.has('empty') ? Array(14).fill(0) : [120, 340, 0, 80, 410, 655, 230, 0, 0, 512, 380, 720, 290, 146];
  const stats = q.has('empty')
    ? { dictations: 0, words: 0, speaking_ms: 0, words_today: 0, dictations_today: 0, saved_secs: 0, avg_wpm: 0, streak_days: 0, top_apps: [], daily }
    : { dictations: 412, words: 12431, speaking_ms: 5160000, words_today: 146, dictations_today: 6, saved_secs: 13485, avg_wpm: 144, streak_days: 4,
        top_apps: [['Outlook', 4210], ['Slack', 3388], ['Chrome', 2105], ['Code', 1690], ['Notion', 1038]], daily };
  const providers = [{ id: 'gemini', name: 'Gemini', url: 'https://gemini.google.com/app' }, { id: 'chatgpt', name: 'ChatGPT', url: 'https://chatgpt.com/' }];
  const handlers = {};
  const wait = (ms) => new Promise((r) => setTimeout(r, ms));
  const calls = (window.__calls = []);

  async function invoke(cmd, args = {}) {
    calls.push([cmd, args]);
    switch (cmd) {
      case 'get_state':
        return structuredClone({ settings, stats, providers, loaded_provider: settings.provider,
          hotkey_error: q.has('hotkeyerror') ? 'Ctrl+Shift+Space is already used by another program' : null,
          data_dir: 'C:\\Users\\you\\AppData\\Roaming\\VillFlowV4', version: '0.7.0' });
      case 'save_settings':
        if (args.settings.hotkey === args.settings.command_hotkey) throw 'The dictation and command hotkeys must be different';
        Object.assign(settings, structuredClone(args.settings));
        return null;
      case 'history': {
        const qq = (args.query || '').toLowerCase();
        return entries
          .filter((e) => !qq || e.text.toLowerCase().includes(qq) || e.note.toLowerCase().includes(qq))
          .filter((e) => !args.kind || (args.kind === 'dictation' ? e.kind === 'dictation' : e.kind !== 'dictation'))
          .slice(args.offset, args.offset + args.limit);
      }
      case 'delete_entry': entries = entries.filter((e) => e.id !== args.id); return null;
      case 'clear_history': entries = []; return null;
      case 'provider_status':
        await wait(600);
        if (q.has('signedout') && args.id === 'chatgpt') return { ok: true, ready: false, signed_out: true, composer: false };
        return { ok: true, ready: true, signed_out: false, composer: true, mic_button: true };
      case 'test_dictation':
        setTimeout(() => handlers['vf://dictation']?.({ payload: { text: 'Testing one two three, this is the microphone check.' } }), (args.seconds + 1.5) * 1000);
        return null;
      case 'provider_inspect': return { ok: true, buttons: ['Dictate', 'Send message'] };
      case 'customize_selectors': return 'C:\\Users\\you\\AppData\\Roaming\\VillFlowV4\\providers.json';
      default: return null;
    }
  }
  window.__TAURI__ = {
    core: { invoke },
    event: { listen: async (name, fn) => { handlers[name] = fn; return () => {}; } },
  };
  window.__emit = (name, payload) => handlers[name]?.({ payload });
  // ?click=<selector>: click something after load (for screenshots of dialogs etc.)
  if (q.has('click')) setTimeout(() => document.querySelector(q.get('click'))?.click(), 900);
  // ?css=<rules>: extra CSS (e.g. force hover-only controls visible); ?error=<text>: show an error event
  if (q.has('css')) addEventListener('DOMContentLoaded', () => document.head.insertAdjacentHTML('beforeend', `<style>${q.get('css')}</style>`));
  if (q.has('error')) setTimeout(() => window.__emit('vf://error', q.get('error')), 900);
})();
