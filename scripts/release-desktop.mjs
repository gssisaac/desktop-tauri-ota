#!/usr/bin/env node
/**
 * Desktop launcher release: build the signed DMG (launcher + bundled Node.js runtime), enforce
 * the 70 MB size budget, and publish it to Cloudflare R2 under downloads/ plus
 * downloads/latest.json (read by __APP_DOMAIN__/download and /download/mac).
 *
 * Usage: pnpm release:desktop [--dry-run]
 * Signing: scripts/deploy/apple-signing.env (APPLE_SIGNING_IDENTITY), else the first
 * "Developer ID Application" identity in the keychain.
 * Notarization: Tauri notarizes the .app; scripts/deploy/notarize-dmg.sh staples the DMG.
 * Credentials are required (App Store Connect API key, or Apple ID + app-specific password).
 */
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  NODE_MODULE_VERSION,
  NODE_VERSION,
  hostTargetTriple,
  prepareNodeSidecar,
} from './prepare-node-sidecar.mjs';
import { parseEnvFile } from './ota-credentials.mjs';
import { r2BaseUrl, r2Put } from './r2.mjs';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const dryRun = process.argv.includes('--dry-run');

/**
 * Hard budget for the installer: the whole app ships via OTA, the DMG is only the launcher plus
 * its Node.js runtime (Contents/MacOS/node).
 */
const MAX_DMG_BYTES = 70_000_000;

const desktopRoot = join(root, 'desktop');
const tauriConf = JSON.parse(
  readFileSync(join(desktopRoot, 'src-tauri', 'tauri.conf.json'), 'utf8'),
);
const version = tauriConf.version;
const arch = process.arch === 'arm64' ? 'aarch64' : 'x64';
const bundleDir = join(desktopRoot, 'src-tauri', 'target', 'release', 'bundle');
const appPath = join(bundleDir, 'macos', `${tauriConf.productName}.app`);
const dmgName = `${tauriConf.productName}_${version}_${arch}.dmg`;
const dmgPath = join(bundleDir, 'dmg', dmgName);
const staging = join(root, '.desktop-staging');

function fail(message) {
  console.error(`[release-desktop] ${message}`);
  process.exit(1);
}

function run(cmd, args) {
  return spawnSync(cmd, args, { encoding: 'utf8' });
}

function expandEnvRefs(value) {
  return value.replace(/\$\{([A-Z0-9_]+)\}/g, (_, name) => process.env[name] ?? '');
}

/** Load gitignored scripts/deploy/apple-signing.env without overriding the shell. */
function loadAppleSigningEnv() {
  const file = join(root, 'scripts', 'deploy', 'apple-signing.env');
  if (!existsSync(file)) {
    fail(
      'missing scripts/deploy/apple-signing.env — copy scripts/deploy/apple-signing.env.example and fill in Developer ID + notarization credentials',
    );
  }
  const parsed = parseEnvFile(file);
  for (const [key, value] of Object.entries(parsed)) {
    if (!process.env[key]) process.env[key] = value;
  }
  for (const key of Object.keys(parsed)) {
    if (process.env[key]) process.env[key] = expandEnvRefs(process.env[key]);
  }
  const keyPath = process.env.APPLE_API_KEY_PATH?.trim();
  if (keyPath && !keyPath.startsWith('/')) {
    process.env.APPLE_API_KEY_PATH = join(dirname(file), keyPath);
  }
  console.log(`[release-desktop] loaded Apple signing env from ${file}`);
}

function hasNotaryAuth() {
  if (process.env.NOTARYTOOL_PROFILE?.trim()) return true;
  const keyPath = process.env.APPLE_API_KEY_PATH?.trim();
  if (
    keyPath &&
    existsSync(keyPath) &&
    process.env.APPLE_API_KEY?.trim() &&
    process.env.APPLE_API_ISSUER?.trim()
  ) {
    return true;
  }
  return Boolean(
    process.env.APPLE_ID?.trim() &&
      process.env.APPLE_PASSWORD?.trim() &&
      process.env.APPLE_TEAM_ID?.trim(),
  );
}

loadAppleSigningEnv();
if (!hasNotaryAuth()) {
  fail(
    'notarization credentials missing in scripts/deploy/apple-signing.env (API key or Apple ID + app-specific password)',
  );
}

// Tauri notarizes the .app when the API key vars are set. Drop Apple ID auth so it
// does not prefer an app-specific password over the API key.
const buildEnv = { ...process.env };
if (
  buildEnv.APPLE_API_ISSUER &&
  buildEnv.APPLE_API_KEY &&
  buildEnv.APPLE_API_KEY_PATH
) {
  delete buildEnv.APPLE_ID;
  delete buildEnv.APPLE_PASSWORD;
  delete buildEnv.APPLE_APP_SPECIFIC_PASSWORD;
  delete buildEnv.NOTARYTOOL_PROFILE;
}

// 1. Signing identity.
let identity = process.env.APPLE_SIGNING_IDENTITY?.trim();
if (!identity) {
  const found = run('security', ['find-identity', '-v', '-p', 'codesigning']).stdout ?? '';
  identity = found.match(/"(Developer ID Application: [^"]+)"/)?.[1];
}
if (!identity) fail('no "Developer ID Application" signing identity found in the keychain');
console.log(`[release-desktop] signing as ${identity}`);

// 2. Build the launcher .app + .dmg with the Node.js sidecar (tauri.conf.json externalBin).
try {
  await prepareNodeSidecar(hostTargetTriple());
} catch (err) {
  fail(`Node.js sidecar: ${err instanceof Error ? err.message : err}`);
}
rmSync(dmgPath, { force: true });
buildEnv.APPLE_SIGNING_IDENTITY = identity;
const build = spawnSync('pnpm', ['--dir', desktopRoot, 'run', 'build'], {
  cwd: root,
  stdio: 'inherit',
  env: buildEnv,
});
if (build.status !== 0) fail('tauri build failed');
if (!existsSync(dmgPath)) fail(`expected ${dmgPath}`);

// Tauri notarizes the .app. The DMG needs its own notarytool submit + staple.
const notarize = spawnSync('bash', [join(root, 'scripts', 'deploy', 'notarize-dmg.sh')], {
  cwd: root,
  stdio: 'inherit',
  env: buildEnv,
});
if (notarize.status !== 0) fail('DMG notarization failed');

// 3. Size budget.
const size = statSync(dmgPath).size;
console.log(`[release-desktop] ${dmgName}: ${(size / 1_000_000).toFixed(2)} MB`);
if (size > MAX_DMG_BYTES) {
  fail(`DMG is ${(size / 1_000_000).toFixed(2)} MB — over the ${MAX_DMG_BYTES / 1_000_000} MB budget`);
}

// 4. Signature + notarization status, and the signed runtime still runs (hardened runtime + JIT).
const verify = run('codesign', ['--verify', '--deep', '--strict', appPath]);
if (verify.status !== 0) fail(`codesign verification failed:\n${verify.stderr}`);
const runtime = run(join(appPath, 'Contents', 'MacOS', 'node'), [
  '-p',
  'process.versions.node + " " + process.versions.modules',
]);
if (runtime.stdout?.trim() !== `${NODE_VERSION} ${NODE_MODULE_VERSION}`) {
  fail(`bundled Node.js runtime check failed: ${runtime.error?.message ?? (runtime.stderr || runtime.stdout)}`);
}
console.log(`[release-desktop] bundled Node v${NODE_VERSION} (ABI ${NODE_MODULE_VERSION}) runs`);
const assessment = run('spctl', ['--assess', '--type', 'execute', '-vv', appPath]);
const dmgAssessment = run('spctl', ['-a', '-vv', '-t', 'install', dmgPath]);
const notarized = /source=Notarized Developer ID/.test(
  `${assessment.stdout}${assessment.stderr}${dmgAssessment.stdout}${dmgAssessment.stderr}`,
);
console.log(`[release-desktop] signature ok · notarized: ${notarized}`);
if (!notarized) {
  fail(
    `Gatekeeper does not see a notarized Developer ID.\napp: ${assessment.stderr || assessment.stdout}\ndmg: ${dmgAssessment.stderr || dmgAssessment.stdout}`,
  );
}

// 5. Publish under the DMG's own file name. A name is one file, so there is no hash folder.
const dmgBytes = readFileSync(dmgPath);
const sha256 = createHash('sha256').update(dmgBytes).digest('hex');
const key = `downloads/${dmgName}`;
const latest = {
  version,
  arch,
  minimumSystemVersion: tauriConf.bundle?.macOS?.minimumSystemVersion ?? null,
  url: `${r2BaseUrl}/${key}`,
  size,
  sha256,
  notarized,
  releaseDate: new Date().toISOString(),
};

rmSync(staging, { recursive: true, force: true });
mkdirSync(staging, { recursive: true });
const latestPath = join(staging, 'latest.json');
writeFileSync(latestPath, `${JSON.stringify(latest, null, 2)}\n`);
console.log('[release-desktop] latest.json:', latest);

if (dryRun) {
  console.log('[release-desktop] --dry-run: skipping upload');
  process.exit(0);
}

if (
  !r2Put(key, dmgPath, 'application/x-apple-diskimage', 'public, max-age=31536000, immutable', {
    contentDisposition: `attachment; filename="${dmgName}"`,
  })
) {
  fail('DMG upload failed. Fix auth (.env or wrangler login) and retry.');
}
if (!r2Put('downloads/latest.json', latestPath, 'application/json', 'no-cache')) {
  fail('latest.json upload failed after the DMG was uploaded; rerun to publish it.');
}

console.log('[release-desktop] done →', latest.url);
