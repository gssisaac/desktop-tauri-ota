#!/usr/bin/env node
/**
 * One-time helper when refreshing from a product repo. Replaces product-specific strings
 * with boilerplate tokens. Not run in consumer projects — agents replace __APP_*__ manually.
 */
import { readFileSync, writeFileSync, readdirSync, statSync } from 'node:fs';
import { join, extname } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(fileURLToPath(import.meta.url), '..', '..');
const SKIP = new Set(['neutralize-placeholders.mjs', 'Cargo.lock']);
const TEXT_EXT = new Set([
  '.rs', '.json', '.html', '.md', '.mjs', '.sh', '.toml', '.plist', '.example', '.gitignore',
]);

const PLACEHOLDER_PUBLIC_KEY =
  '0000000000000000000000000000000000000000000000000000000000000000';

function walk(dir, files = []) {
  for (const name of readdirSync(dir)) {
    if (name === 'node_modules' || name === 'target' || name === 'binaries') continue;
    const full = join(dir, name);
    if (statSync(full).isDirectory()) walk(full, files);
    else files.push(full);
  }
  return files;
}

function transform(content, file) {
  let s = content;
  s = s.replace(/tablepad_desktop_lib/g, '__APP_SLUG_SNAKE___lib');
  s = s.replace(/tablepad_desktop/g, '__APP_SLUG_SNAKE__');
  s = s.replace(/dev\.tablepad\.desktop/g, '__APP_IDENTIFIER__');
  s = s.replace(/https:\/\/bundles\.tablepad\.dev/g, 'https://bundles.__APP_DOMAIN__');
  s = s.replace(/bundles\.tablepad\.dev/g, 'bundles.__APP_DOMAIN__');
  s = s.replace(/tablepad-bundles/g, '__APP_SLUG__-bundles');
  s = s.replace(/tablepad\.dev/g, '__APP_DOMAIN__');
  s = s.replace(/TablePad/g, '__APP_NAME__');
  s = s.replace(/tablepad_data_dir/g, 'app_data_dir');
  s = s.replace(/tablepad-/g, '__APP_SLUG__-');
  s = s.replace(/tablepad/g, '__APP_SLUG__');
  s = s.replace(/TABLEPAD/g, '__APP_SLUG_UPPER__');
  s = s.replace(/47147/g, '__APP_PORT_PROD__');
  s = s.replace(/47144/g, '__APP_PORT_DEV__');
  s = s.replace(
    /CLOUDFLARE_ACCOUNT_ID: process\.env\.CLOUDFLARE_ACCOUNT_ID \|\| '[^']+'/,
    'CLOUDFLARE_ACCOUNT_ID: process.env.CLOUDFLARE_ACCOUNT_ID',
  );
  if (file.endsWith('crypto.rs')) {
    s = s.replace(/"[0-9a-f]{64}"/g, `"${PLACEHOLDER_PUBLIC_KEY}"`);
  }
  return s;
}

for (const file of walk(root)) {
  const base = file.split('/').pop();
  if (SKIP.has(base)) continue;
  const ext = extname(file);
  if (!TEXT_EXT.has(ext) && !base.endsWith('package.json')) continue;
  const before = readFileSync(file, 'utf8');
  const after = transform(before, file);
  if (after !== before) writeFileSync(file, after);
}

console.log('[neutralize] done');
