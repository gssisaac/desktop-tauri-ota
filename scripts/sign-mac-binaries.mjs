/**
 * Developer ID-sign native binaries inside an OTA bundle directory before it is zipped into
 * the DMG seed. Apple notarization unpacks nested bundle.zip in Resources and rejects unsigned
 * .node / .dylib / helper executables.
 *
 * No identity (local `pnpm build:desktop`) skips signing and warns. `pnpm release:desktop`
 * puts APPLE_SIGNING_IDENTITY in the environment before the seed build, so that path signs.
 */
import { spawnSync } from 'node:child_process';
import { existsSync, readdirSync, statSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseEnvFile } from './ota-credentials.mjs';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');

function loadAppleSigningIdentity() {
  const file = join(root, 'scripts', 'deploy', 'apple-signing.env');
  if (existsSync(file)) {
    const parsed = parseEnvFile(file);
    for (const [key, value] of Object.entries(parsed)) {
      if (!process.env[key]) process.env[key] = value;
    }
  }
  let identity = process.env.APPLE_SIGNING_IDENTITY?.trim();
  if (!identity) {
    const found = spawnSync('security', ['find-identity', '-v', '-p', 'codesigning'], { encoding: 'utf8' });
    identity = found.stdout?.match(/"(Developer ID Application: [^"]+)"/)?.[1];
  }
  return identity ?? null;
}

function shouldSign(name) {
  if (name.endsWith('.node') || name.endsWith('.dylib')) return true;
  if (name === 'spawn-helper') return true;
  return false;
}

function walk(dir, files = []) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) walk(full, files);
    else if (shouldSign(entry.name)) files.push(full);
  }
  return files;
}

/** @returns {number} count of binaries signed */
export function signMacBundleNativeBinaries(bundleDir) {
  if (process.platform !== 'darwin') return 0;
  const identity = loadAppleSigningIdentity();
  if (!identity) {
    console.warn(
      '[sign-mac] no Developer ID identity; native modules inside the installer seed are unsigned (notarization will reject the DMG)',
    );
    return 0;
  }
  const files = walk(bundleDir);
  for (const file of files) {
    if (!statSync(file).isFile()) continue;
    const result = spawnSync(
      'codesign',
      ['--force', '--options', 'runtime', '--timestamp', '--sign', identity, file],
      { encoding: 'utf8' },
    );
    if (result.status !== 0) {
      throw new Error(`codesign failed for ${file}:\n${result.stderr || result.stdout}`);
    }
  }
  if (files.length > 0) {
    console.log(`[sign-mac] signed ${files.length} native binaries for installer seed`);
  }
  return files.length;
}
