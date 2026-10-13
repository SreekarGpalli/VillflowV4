# VillFlow V4

[![CI](https://github.com/SreekarGpalli/VillflowV4/actions/workflows/ci.yml/badge.svg)](https://github.com/SreekarGpalli/VillflowV4/actions/workflows/ci.yml)
[![Latest release](https://img.shields.io/github/v/release/SreekarGpalli/VillflowV4)](https://github.com/SreekarGpalli/VillflowV4/releases/latest)

**Free push-to-talk dictation for Windows, powered by the Gemini or ChatGPT account you already have.**
Hold a hotkey, speak, and let go. The text appears wherever your cursor is.

**[Download the latest release](https://github.com/SreekarGpalli/VillflowV4/releases/latest)**

There are no API keys and no extra cost. VillFlow uses the dictation (mic) button inside your own
signed-in Gemini or ChatGPT web app, copies the transcript out *before anything is sent*,
and pastes it into whatever app you're using. Plain dictation never sends a message.

A second hotkey turns the AI into a voice-driven writing tool: change selected text, ask about it,
continue it, ask a question, or have it write something, all in a temporary chat that isn't saved
to your chat history. You can dictate with one and write with the other (e.g. Gemini hears you,
ChatGPT writes).

## Features

- **Push-to-talk anywhere**: hold `Ctrl+Shift+Space` (configurable), or use toggle mode (press to start, press again to stop).
- **Command mode** (`Ctrl+Shift+X`), for text you've selected:
  - *change it* ("make this more formal", "fix the grammar", "translate to Hindi"): the selection is replaced;
  - *ask about it* ("what's the main point?", "summarize this", "write a reply to this"): the answer goes
    below it, your text stays;
  - *continue it* ("add one more sentence"): the new part goes right after it.

  With nothing selected: ask a question ("what's the capital of Australia?") or say what to write
  ("a short message telling my team the server is back online"); the result is typed at the cursor.
- **Roles**: pick who handles dictation and who handles commands (Accounts page). Requests run in that
  site's temporary chat, never saved to your history; VillFlow refuses to send if temporary mode can't be
  confirmed. Typical time: 4 to 10 seconds, mostly the model answering.
- **Dictionary**: your spellings for names and terms ("villa flow" → "VillFlow"), fixed in every dictation
  and passed to the AI when it writes.
- **Polish** (optional): the commands account removes "um"s and false starts and fixes punctuation before pasting. Adds a few seconds.
- **Pick your provider**: Gemini (the default, and usually the most accurate) or ChatGPT. Switch from the tray menu any time.
- **Paste, Type or Copy**: paste is fastest, Type works in apps that block paste. Your clipboard is restored after pasting, including copied images and files, and the dictation never shows up in Win+V clipboard history.
- **Never loses what you said, never pastes into the wrong app**: every transcript is saved before it's pasted. If pasting fails (e.g. into an admin window), or you switched to another window while it was working, the text is left on your clipboard instead.
- **Back-to-back dictation**: press the hotkey again while the previous one is still transcribing and the next one starts as soon as it's done.
- **History & stats**: searchable transcripts (filter dictations vs. commands), time saved vs. typing, words per day for the last two weeks, speaking pace, day streak and top apps. Everything stays on your PC.
- **Floating status pill**: shows *Starting… → Listening · 0:04 → Transcribing…*, with live text when the provider streams it. Anti-aliased, drawn in software (no GPU), and sized per monitor so it looks right at 100% and at 250% scaling.
- **Light on resources**: a 5.5 MB native Rust app (~30 MB RAM) plus one provider page that is trimmed to almost nothing when idle. It can optionally be unloaded completely after 10, 30 or 60 idle minutes.
- **Self-healing**: closes cookie banners and "what's new" popups, reloads the page if it crashed or went stale, retries while it's loading, and never leaves the mic running after a session.

## Install

Download `VillFlow-V4_<version>_x64-setup.exe` from [Releases](https://github.com/SreekarGpalli/VillflowV4/releases/latest)
and run it. It installs for your user only (no admin prompt), adds Start menu and Desktop shortcuts, and lives
side by side with older VillFlow versions. There's also a `-portable.exe` that runs without installing.
Requires Windows 10/11 with the WebView2 runtime (preinstalled on Windows 11).

The builds aren't code-signed, so Windows SmartScreen may say "Windows protected your PC" the first time:
click **More info → Run anyway**. Each release lists SHA-256 checksums in `SHA256SUMS.txt`.

Turn on **Settings → Start with Windows** and VillFlow starts quietly in the tray when you sign in.

## First run

1. **Home → Finish setting up → Open Gemini**: a normal window opens. Sign in and close it.
   You only do this once; the login is remembered. (ChatGPT: **Accounts → Open ChatGPT**.)
2. **Test dictation** records five seconds and shows what came back.
3. Click into any text box anywhere, hold the hotkey, talk, and release.

## How it works

```
hotkey (WH_KEYBOARD_LL) ──► dictation session ──► driver.js inside the provider page
                                                    • clicks the site's own "Dictate" button
                                                    • the page records your mic itself
                                                    • clicks Stop / Confirm, waits for the text
                                                    • reads it, clears the box (never sends)
                    paste / type / copy ◄── transcript ◄──┘
```

- The provider page lives in a WebView2 window that is **DWM-cloaked**: invisible, takes no clicks,
  not in the taskbar or Alt+Tab, but still rendered at full size so the site behaves normally.
- Microphone permission is auto-granted **only** to the provider's own domain.
- A hard guard in the driver refuses to click any button whose label contains "send"
  (e.g. ChatGPT's *Transcribe and send*) or is "Submit". Your dictation can never be posted as a chat.

## Getting clean text back from a chat app

Chat apps render answers as rich text (bold, lists, tables, images), which doesn't copy cleanly. VillFlow's
prompt makes the model answer inside **one plain-text code block**, which chat apps show
character-for-character, and reads exactly that block. On top of that, the prompt has strict plain-text rules:
no markdown, no tables (rows become plain lines), no images (a drawing request gets ASCII art or a
description), no preamble or quotes. Edit requests start with `REPLACE`, `APPEND` or `ANSWER`, so VillFlow
knows whether to overwrite, extend or keep your selection; if the marker is missing it never overwrites.
If a model ignores the format, the rendered text is used (after a longer settle, in case the code block is
still coming); screen-reader-only labels such as "ChatGPT said:" are never taken as the answer; if an answer
is only an image, VillFlow says so right away.

Tested live (`node tests/writing-matrix.mjs chatgpt|gemini [--hard]` with a debug build running):
17/17 everyday cases and 7/7 hard cases on both ChatGPT and Gemini (October 2026). The hard cases are prompt injection
inside the selection, selections containing code fences, long selections, requests for markdown tables
or images, Hindi instructions, and "continue this".

## When a site changes its layout

These sites redesign from time to time. If dictation stops working:

1. **Accounts → Troubleshooting → Inspect** shows every button and text box VillFlow can see.
2. **Edit patterns** opens `%APPDATA%\VillFlowV4\providers.json`. Each provider has lists of patterns:
   `css:<selector>` or `re:<regex>` (matched against a button's aria-label, title, test id, tooltip and text).

   | key | meaning |
   | --- | --- |
   | `composer` | CSS selectors for the message box |
   | `start` / `stop` / `cancel` | the dictation buttons |
   | `recording` | buttons that only exist while recording |
   | `busy` | "transcribing…" indicators |
   | `signed_out` | buttons that mean you're logged out |
   | `exclude` | buttons never to treat as dictation (voice mode, etc.) |
   | `dismiss` | extra popup-closing buttons for this site |
   | `start_shortcut` | keyboard fallback, e.g. Gemini `Ctrl+Shift+D` |
   | `toggle_stop`, `restart_on_silence`, `warmup_ms`, `settle_ms`, `empty_timeout_ms`, `timeout_ms` | behavior tuning |
3. **Reload patterns** applies the changes without restarting. **Reset to defaults** removes your overrides.
   Only `gemini` and `chatgpt` entries are used; anything else in the file is ignored.

## Good to know

- **Terms of service**: automating these consumer websites isn't something their terms allow. VillFlow keeps
  usage human-scale (one dictation at a time, triggered by you), but use it with your own account at your own discretion.
- **Google sign-in**: Google sometimes blocks sign-in inside embedded app windows. If that happens, use
  the site's email login where possible.
- **Two accounts, two pages**: when dictation and commands use different accounts, a second page stays
  loaded (trimmed when idle). In terminals, selections aren't
  read (Ctrl+C would interrupt the running program), so the command hotkey always writes new text there.
- **Admin apps**: Windows doesn't let a normal app paste into windows running as Administrator. VillFlow then leaves the text on your clipboard and says so.
- **Hotkey sources**: the hotkey works from a real keyboard, remote desktop, on-screen keyboards and AutoHotkey remaps. Line breaks are typed as Shift+Enter in Type mode, so chat apps don't send early.
- **Grok** was supported until 0.6. It's gone in 0.7 (its pop-ups and dictation flow were unreliable);
  a saved Grok setting switches to Gemini on first start.

## Development

```bash
npm install          # Tauri CLI
npm run dev          # run in development
npm run build        # release exe + NSIS installer
cd src-tauri && cargo test && cargo clippy --all-targets
node tests/driver-test.mjs   # driver.js vs mock ChatGPT/Gemini pages in headless Edge
node tests/ui-preview.mjs    # the settings UI with a mocked backend, in tests/out/ui (see the file for URL options)
node tests/writing-matrix.mjs chatgpt [--hard]   # live writing tests (debug build with --remote-debugging-port=9333)
```

### Releasing

1. Bump the version in `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml` and `package.json`.
2. Add a `## [x.y.z] - date` section to `CHANGELOG.md`.
3. Commit, then tag and push: `git tag vx.y.z && git push origin main vx.y.z`.

The **Release** workflow builds the installer and portable exe on GitHub, writes `SHA256SUMS.txt`, and
publishes the release with that version's changelog section as its notes. CI runs formatting, clippy, unit
tests and the driver safety tests on every push.

`tests/driver-test.mjs` checks the safety-critical page automation: the right dictation buttons are
clicked (never voice mode), nothing that sends a message is ever clicked even if the selector config
asks for it, popups are only ever declined or closed, and transcripts and site errors are read correctly.

Developer flags: `--print-providers` dumps the built-in selectors, `--signin-all` opens both
sign-in windows, `--background` starts in the tray. Debug builds also have `--test-deliver <paste|type|copy> <text>`.

Code map (`src-tauri/src`):

| file | role |
| --- | --- |
| `main.rs` | app setup, tray, IPC commands for the UI, launch at login |
| `dictation.rs` | one hotkey session: start, record, stop, wait for transcript, polish/edit/write, deliver, record history |
| `assist.rs` | asking the provider to write (temporary chat, fast model, restore model), prompts, personal dictionary |
| `providers.rs` | provider configs (defaults + `providers.json`), cloaked WebView2 window, mic permission, script bridge |
| `driver.js` | runs inside the provider page: find buttons, start/stop dictation, read and clear the box, close popups |
| `hotkeys.rs` | low-level keyboard hook (push-to-talk needs key-up), physical modifier tracking |
| `inject.rs` | paste / type / copy, focus restore, foreground app name |
| `overlay.rs` | native status pill (per-pixel-alpha layered window) |
| `idle.rs` | low-memory mode, idle unload, periodic page refresh |
| `store.rs` | settings JSON + SQLite history and stats |

The UI (`ui/`) is plain HTML/CSS/JS with no build step. Design rules it follows, so it stays consistent:
one accent (the icon's green) and neutral surfaces; color only for state (ok / warning / error, always
with a text label); Segoe UI Variable to match Windows 11; one line-icon set at one stroke width; no
emoji, gradients, glows or nested cards; motion only for state changes (120-150 ms, off with reduced
motion); every control has hover, focus and disabled states; light and dark themes follow Windows.

Data lives in `%APPDATA%\VillFlowV4` (settings, history, selector overrides) and
`%LOCALAPPDATA%\com.villflow.v4` (the WebView2 profile with your logins).
