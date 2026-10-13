// Copies ui/ into tests/out/ui with a mocked Tauri backend (tests/ui-mock.js),
// so the settings window can be checked in any browser:
//   node tests/ui-preview.mjs && python -m http.server 8765 -d tests/out
//   then open http://127.0.0.1:8765/ui/  (add ?fresh, ?empty, ?signedout, ?hotkeyerror, ?cmdoff)
import { copyFileSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const out = join(root, 'tests', 'out', 'ui');
mkdirSync(out, { recursive: true });
for (const f of ['styles.css', 'app.js']) copyFileSync(join(root, 'ui', f), join(out, f));
copyFileSync(join(root, 'tests', 'ui-mock.js'), join(out, 'ui-mock.js'));
const html = readFileSync(join(root, 'ui', 'index.html'), 'utf8')
  .replace('<script src="app.js"></script>', '<script src="ui-mock.js"></script>\n  <script src="app.js"></script>');
writeFileSync(join(out, 'index.html'), html);
console.log(`preview ready in ${out}`);
