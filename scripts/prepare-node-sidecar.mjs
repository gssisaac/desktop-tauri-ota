#!/usr/bin/env node
/**
 * Fetch the Node.js runtime the desktop launcher ships as a Tauri sidecar
 * (`bundle.externalBin: binaries/node` → __APP_NAME__.app/Contents/MacOS/node), so OTA bundles
 * always run on the Node ABI they were built for, whatever the host has installed.
 *
 * Only `bin/node` (`node.exe` on Windows) is kept, after checking the archive against
 * nodejs.org's SHASUMS256.txt. Already-prepared binaries are reused (offline-friendly), keyed
 * by a version stamp.
 *
 * Usage: node scripts/prepare-node-sidecar.mjs [--target <triple>|all]
 * Default target: this machine (aarch64-apple-darwin, x86_64-apple-darwin or
 * x86_64-pc-windows-msvc). `all` prepares both macOS targets.
 */
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import {
  chmodSync,
  copyFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  renameSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

/** Node 22 LTS. Bump the patch freely; a new major changes the ABI of every OTA bundle. */
export const NODE_VERSION = '22.23.3';
/** process.versions.modules of NODE_VERSION; native modules in OTA bundles must match it. */
export const NODE_MODULE_VERSION = '127';

const DIST = `https://nodejs.org/dist/v${NODE_VERSION}`;
const ARCH_BY_TRIPLE = {
  'aarch64-apple-darwin': 'arm64',
  'x86_64-apple-darwin': 'x64',
  'x86_64-pc-windows-msvc': 'x64',
};
const MACOS_TRIPLES = ['aarch64-apple-darwin', 'x86_64-apple-darwin'];

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
export const sidecarDir = join(root, 'desktop', 'src-tauri', 'binaries');

const isWindowsTriple = (triple) => triple.endsWith('-windows-msvc');

export function hostTargetTriple() {
  if (process.platform === 'win32' && process.arch === 'x64') return 'x86_64-pc-windows-msvc';
  if (process.platform !== 'darwin') {
    throw new Error(
      `the Node sidecar is only prepared for macOS and Windows x64 (this is ${process.platform}-${process.arch})`,
    );
  }
  return process.arch === 'arm64' ? 'aarch64-apple-darwin' : 'x86_64-apple-darwin';
}

/** Tauri's `externalBin` naming: `node-<triple>`, plus `.exe` on Windows. */
export function sidecarPath(triple = hostTargetTriple()) {
  return join(sidecarDir, `node-${triple}${isWindowsTriple(triple) ? '.exe' : ''}`);
}

function stampPath(triple) {
  return `${sidecarPath(triple)}.version`;
}

async function download(url) {
  const res = await fetch(url);
  if (!res.ok) throw new Error(`GET ${url} → HTTP ${res.status}`);
  return Buffer.from(await res.arrayBuffer());
}

/** Ensure binaries/node-<triple> is Node NODE_VERSION; returns its path. */
export async function prepareNodeSidecar(triple = hostTargetTriple()) {
  const arch = ARCH_BY_TRIPLE[triple];
  if (!arch) throw new Error(`unsupported target ${triple} (expected ${Object.keys(ARCH_BY_TRIPLE).join(', ')})`);

  const dest = sidecarPath(triple);
  const stamp = stampPath(triple);
  if (existsSync(dest) && existsSync(stamp) && readFileSync(stamp, 'utf8').trim() === NODE_VERSION) {
    return dest;
  }

  const windows = isWindowsTriple(triple);
  const name = `node-v${NODE_VERSION}-${windows ? 'win' : 'darwin'}-${arch}`;
  const archiveName = windows ? `${name}.zip` : `${name}.tar.gz`;
  const member = windows ? `${name}/node.exe` : `${name}/bin/node`;
  console.log(`[node-sidecar] downloading ${archiveName}…`);
  const [archiveBytes, sums] = await Promise.all([
    download(`${DIST}/${archiveName}`),
    download(`${DIST}/SHASUMS256.txt`).then(String),
  ]);
  const expected = sums
    .split('\n')
    .map((line) => line.trim().split(/\s+/))
    .find(([, file]) => file === archiveName)?.[0];
  if (!expected) throw new Error(`SHASUMS256.txt has no entry for ${archiveName}`);
  const actual = createHash('sha256').update(archiveBytes).digest('hex');
  if (actual !== expected) throw new Error(`${archiveName} checksum mismatch (${actual} ≠ ${expected})`);

  const work = mkdtempSync(join(tmpdir(), '__APP_SLUG__-node-sidecar-'));
  try {
    const archive = join(work, archiveName);
    const out = join(work, 'out');
    mkdirSync(out);
    writeFileSync(archive, archiveBytes);
    // Windows' bundled bsdtar reads .zip; Git for Windows' GNU tar on PATH does not.
    const tarBin = windows ? join(process.env.SystemRoot ?? 'C:\\Windows', 'System32', 'tar.exe') : 'tar';
    const tar = spawnSync(tarBin, [windows ? '-xf' : '-xzf', archive, '-C', out, member], {
      encoding: 'utf8',
    });
    if (tar.status !== 0) throw new Error(`extracting ${member} failed: ${tar.stderr}`);

    mkdirSync(sidecarDir, { recursive: true });
    const partial = `${dest}.partial`;
    copyFileSync(join(out, ...member.split('/')), partial);
    chmodSync(partial, 0o755);
    renameSync(partial, dest);
    writeFileSync(stamp, `${NODE_VERSION}\n`);
  } finally {
    rmSync(work, { recursive: true, force: true });
  }

  console.log(`[node-sidecar] ${dest} (Node v${NODE_VERSION}, ABI ${NODE_MODULE_VERSION})`);
  return dest;
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  const flag = process.argv.indexOf('--target');
  const requested = flag === -1 ? null : process.argv[flag + 1];
  try {
    const triples =
      requested === 'all' ? MACOS_TRIPLES : [requested ?? hostTargetTriple()];
    for (const triple of triples) await prepareNodeSidecar(triple);
  } catch (err) {
    console.error('[node-sidecar]', err instanceof Error ? err.message : err);
    process.exit(1);
  }
}
