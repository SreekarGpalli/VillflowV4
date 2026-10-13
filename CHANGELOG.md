# Changelog

## [0.7.0] - 2026-10-13

First public release of VillFlow V4.

**Download:** `VillFlow-V4_0.7.0_x64-setup.exe` installs per user (no admin needed), with Start menu and Desktop shortcuts.
`VillFlow-V4_0.7.0_x64-portable.exe` runs without installing. Both need Windows 10/11 with the WebView2 runtime
(preinstalled on Windows 11). Check the download against `SHA256SUMS.txt` if you like.

### What it does
- Push-to-talk dictation in any app, using the dictation button of your own signed-in **Gemini** or **ChatGPT**
  web app. No API keys, no extra cost, and dictation never sends a chat message.
- **Command hotkey**: select text and say what to do with it ("make this more formal", "what does this mean?",
  "continue this"), or ask a question / say what to write with nothing selected. Runs in the site's temporary
  chat, so nothing lands in your chat history.
- Dictionary of your own spellings, optional polish, history with search and filters, and stats (time saved,
  words per day, pace, streak, top apps).

### New in 0.7.0
- **Redesigned settings window**: new Home, Accounts and History pages, light and dark themes that follow
  Windows, one consistent icon set, keyboard focus states, custom dialogs and toasts.
- **Home** shows a status headline with the one action that fixes a problem, both shortcuts with examples,
  all-time totals next to a 14-day words-per-day chart, and one-line recent entries.
- **Accounts page**: choose who handles dictation and who handles commands; both accounts' sign-in status is checked.
- **History filters**: All, Dictation or Commands.
- **Status pill** redrawn with anti-aliased edges, a hairline border and a spinner while working; it no longer
  twitches as the timer ticks.
- **Ready at sign-in**: if the network isn't up yet when Windows starts VillFlow, the Gemini/ChatGPT pages are
  checked and reloaded until they're usable.
- **Grok removed.** A saved Grok setting switches to Gemini on first start.

### Fixes
- ChatGPT answers could be read as just "ChatGPT said:" (a hidden screen-reader label) when its Stop button
  flickered. Such labels are ignored now, and the answer is only taken once it has settled.
- Pressing the current hotkey while recording a new one no longer starts a dictation; Escape reliably cancels.
- A settings file saved with a byte-order mark (Notepad, PowerShell) no longer resets your settings.
- Development builds no longer change the Windows sign-in entry of the installed app.

### Tested
19 unit tests, 39 page-automation safety checks, and live writing tests: 17/17 everyday and 7/7 hard cases
on both ChatGPT and Gemini, plus 6/6 spoken commands end to end.

## Earlier versions (not published)

- **0.6.0**: separate writing assistant (e.g. Gemini hears you, ChatGPT writes); answers requested inside one
  plain-text code block and read verbatim; replace / answer-below / continue behaviors; temporary chat enforced.
- **0.5.0**: Gemini became the default; command mode, dictionary and polish ported from V3.
- **0.4.x**: first V4 builds: dictation through the providers' own web apps, global hotkey, paste/type/copy, history and stats.
