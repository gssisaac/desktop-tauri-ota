#!/usr/bin/env node
/**
 * Windows desktop launcher release: build the NSIS installer (launcher + bundled node.exe),
 * enforce the 70 MB size budget, and publish it to Cloudflare R2 under downloads/ plus
 * downloads/latest-windows.json (read by __APP_DOMAIN__/download and /download/windows).
 *
 * Usage: pnpm release:desktop:windows [--dry-run]   (on Windows x64)
 * The installer is not Authenticode-signed yet (`notarized: false`), so the download page shows
 * a SmartScreen hint. The launcher installs engines from manifest-windows-x64.json, which
 * `pnpm release:ota <version>` publishes when run on Windows.
 */
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  NODE_MODULE_VERSION,
  NODE_VERSION,
  hostTargetTriple,
  prepareNodeSidecar,
} from './prepare-node-sidecar.mjs';
import { pnpmSpawn } from './pnpm.mjs';
import { r2BaseUrl, r2Put } from './r2.mjs';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const dryRun = process.argv.includes('--dry-run');

/** Same budget as the DMG: the installer is only the launcher plus its Node.js runtime. */
const MAX_INSTALLER_BYTES = 70_000_000;

const desktopRoot = join(root, 'desktop');
const tauriConf = JSON.parse(
  readFileSync(join(desktopRoot, 'src-tauri', 'tauri.conf.json'), 'utf8'),
);
const version = tauriConf.version;
const arch = 'x64';
const targetDir = process.env.CARGO_TARGET_DIR
  ? resolve(process.env.CARGO_TARGET_DIR)
  : join(desktopRoot, 'src-tauri', 'target');
const releaseDir = join(targetDir, 'release');
const installerName = `${tauriConf.productName}_${version}_${arch}-setup.exe`;
const installerPath = join(releaseDir, 'bundle', 'nsis', installerName);
const staging = join(root, '.desktop-staging');

function fail(message) {
  console.error(`[release-desktop-windows] ${message}`);
  process.exit(1);
}

if (process.platform !== 'win32' || process.arch !== 'x64') {
  fail(`run this on Windows x64 (this is ${process.platform}-${process.arch})`);
}

// 1. Build the launcher + NSIS installer with the Node.js sidecar (tauri.conf.json externalBin).
try {
  await prepareNodeSidecar(hostTargetTriple());
} catch (err) {
  fail(`Node.js sidecar: ${err instanceof Error ? err.message : err}`);
}
rmSync(installerPath, { force: true });
const build = spawnSync(...pnpmSpawn(['--dir', desktopRoot, 'run', 'build']), {
  cwd: root,
  stdio: 'inherit',
});
if (build.status !== 0) fail('tauri build failed');
if (!existsSync(installerPath)) fail(`expected ${installerPath}`);

// 2. Size budget.
const size = statSync(installerPath).size;
console.log(`[release-desktop-windows] ${installerName}: ${(size / 1_000_000).toFixed(2)} MB`);
if (size > MAX_INSTALLER_BYTES) {
  fail(`installer is ${(size / 1_000_000).toFixed(2)} MB — over the ${MAX_INSTALLER_BYTES / 1_000_000} MB budget`);
}

// 3. The runtime Tauri ships beside __APP_NAME__.exe still runs.
const runtime = spawnSync(join(releaseDir, 'node.exe'), [
  '-p',
  'process.versions.node + " " + process.versions.modules',
], { encoding: 'utf8' });
if (runtime.stdout?.trim() !== `${NODE_VERSION} ${NODE_MODULE_VERSION}`) {
  fail(`bundled Node.js runtime check failed: ${runtime.error?.message ?? (runtime.stderr || runtime.stdout)}`);
}
console.log(`[release-desktop-windows] bundled Node v${NODE_VERSION} (ABI ${NODE_MODULE_VERSION}) runs`);

// 4. Publish under the installer's own file name. A name is one file, so there is no hash folder.
const installerBytes = readFileSync(installerPath);
const sha256 = createHash('sha256').update(installerBytes).digest('hex');
const key = `downloads/${installerName}`;
const latest = {
  version,
  arch,
  minimumSystemVersion: '10',
  url: `${r2BaseUrl}/${key}`,
  size,
  sha256,
  notarized: false,
  releaseDate: new Date().toISOString(),
};

rmSync(staging, { recursive: true, force: true });
mkdirSync(staging, { recursive: true });
const latestPath = join(staging, 'latest-windows.json');
writeFileSync(latestPath, `${JSON.stringify(latest, null, 2)}\n`);
console.log('[release-desktop-windows] latest-windows.json:', latest);

if (dryRun) {
  console.log('[release-desktop-windows] --dry-run: skipping upload');
  process.exit(0);
}

if (
  !r2Put(key, installerPath, 'application/vnd.microsoft.portable-executable', 'public, max-age=31536000, immutable', {
    contentDisposition: `attachment; filename="${installerName}"`,
  })
) {
  fail('installer upload failed. Fix auth (.env or wrangler login) and retry.');
}
if (!r2Put('downloads/latest-windows.json', latestPath, 'application/json', 'no-cache')) {
  fail('latest-windows.json upload failed after the installer was uploaded; rerun to publish it.');
}

console.log('[release-desktop-windows] done →', latest.url);
