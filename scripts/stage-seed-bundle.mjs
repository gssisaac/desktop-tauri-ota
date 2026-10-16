#!/usr/bin/env node
/**
 * Copy the latest full-stack bundle into desktop/src-tauri/resources/seed-bundle so the
 * next DMG or NSIS build can launch that version offline.
 *
 * Reuses .ota-staging when it already holds this version and the app source is not newer.
 * Otherwise builds it (`release-ota-bundle.mjs --seed-only`, no upload).
 *
 * Version: {__APP_SLUG_UPPER___BUNDLE_VERSION}, else the staged OTA manifest, else app/package.json.
 *
 * Usage: node scripts/stage-seed-bundle.mjs
 */
import { spawnSync } from 'node:child_process';
import { cpSync, existsSync, mkdirSync, readdirSync, readFileSync, statSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { APP_SLUG, envVar } from './app-config.mjs';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const isWindows = process.platform === 'win32';
const manifestName = isWindows ? 'manifest-windows-x64.json' : 'manifest.json';
const seedDir = join(root, 'desktop', 'src-tauri', 'resources', 'seed-bundle');

function fail(message) {
  console.error(`[stage-seed] ${message}`);
  process.exit(1);
}

export function zipNameFor(version) {
  return `${APP_SLUG}-${version}${isWindows ? '-win-x64' : ''}.zip`;
}

export function stagingBundleVersion() {
  const manifestPath = join(root, '.ota-staging', manifestName);
  if (!existsSync(manifestPath)) return null;
  try {
    const manifest = JSON.parse(readFileSync(manifestPath, 'utf8'));
    return typeof manifest.version === 'string' && manifest.version ? manifest.version : null;
  } catch {
    return null;
  }
}

function appPackageVersion() {
  const pkgPath = join(root, 'app', 'package.json');
  if (!existsSync(pkgPath)) {
    fail('app/package.json is missing — copy the Next.js app before building the installer');
  }
  const pkg = JSON.parse(readFileSync(pkgPath, 'utf8'));
  const version = typeof pkg.version === 'string' ? pkg.version.trim() : '';
  if (!version) fail('app/package.json is missing a version');
  return version;
}

export function resolveSeedVersion() {
  const fromEnv = process.env[envVar('BUNDLE_VERSION')]?.trim();
  if (fromEnv) return fromEnv;
  return stagingBundleVersion() || appPackageVersion();
}

function assertVersion(version) {
  if (!/^[0-9A-Za-z.+_-]{1,64}$/.test(version)) {
    fail(`invalid bundle version "${version}"`);
  }
}

function newestMtime(dir) {
  let newest = 0;
  if (!existsSync(dir)) return newest;
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    if (entry.name === 'node_modules' || entry.name === '.next' || entry.name === '.next-standalone' || entry.name === '.open-next') {
      continue;
    }
    const full = join(dir, entry.name);
    if (entry.isDirectory()) newest = Math.max(newest, newestMtime(full));
    else newest = Math.max(newest, statSync(full).mtimeMs);
  }
  return newest;
}

export function stageSeedBundle(version) {
  assertVersion(version);
  mkdirSync(seedDir, { recursive: true });

  const stagedZip = join(root, '.ota-staging', zipNameFor(version));
  const stagedVersion = stagingBundleVersion();
  const zipFresh =
    existsSync(stagedZip) &&
    statSync(stagedZip).size > 0 &&
    statSync(stagedZip).mtimeMs >= newestMtime(join(root, 'app'));
  if (stagedVersion === version && zipFresh) {
    cpSync(stagedZip, join(seedDir, 'bundle.zip'));
    writeFileSync(join(seedDir, 'version'), `${version}\n`);
    console.log(
      `[stage-seed] installer seed v${version} from .ota-staging (${(statSync(stagedZip).size / 1_048_576).toFixed(1)} MB)`,
    );
    return version;
  }

  console.log(`[stage-seed] building bundle v${version} into the installer…`);
  const build = spawnSync(process.execPath, [join(root, 'scripts', 'release-ota-bundle.mjs'), version, '--seed-only'], {
    cwd: root,
    stdio: 'inherit',
  });
  if (build.status !== 0) fail('seed bundle build failed');

  const versionFile = join(seedDir, 'version');
  const zipFile = join(seedDir, 'bundle.zip');
  if (!existsSync(versionFile) || !existsSync(zipFile)) {
    fail('seed bundle files were not written');
  }
  const written = readFileSync(versionFile, 'utf8').trim();
  if (written !== version) fail(`seed version is ${written}, expected ${version}`);
  return version;
}

/** Locate version + bundle.zip Tauri copied into a built app or release dir. */
export function findBundledSeedVersion(roots) {
  const suffixes = [join('resources', 'seed-bundle', 'version'), join('seed-bundle', 'version')];
  for (const rootDir of roots) {
    for (const suffix of suffixes) {
      const file = join(rootDir, suffix);
      if (!existsSync(file)) continue;
      const zip = join(dirname(file), 'bundle.zip');
      if (!existsSync(zip) || statSync(zip).size === 0) continue;
      return { file, zip, version: readFileSync(file, 'utf8').trim() };
    }
  }
  return null;
}

const isDirectRun =
  process.argv[1] != null &&
  import.meta.url === pathToFileURL(resolve(process.argv[1])).href;
if (isDirectRun) {
  const tauriConf = JSON.parse(readFileSync(join(root, 'desktop', 'src-tauri', 'tauri.conf.json'), 'utf8'));
  const version = resolveSeedVersion();
  if (version !== tauriConf.version) {
    console.log(`[stage-seed] bundle v${version} (launcher is ${tauriConf.version})`);
  }
  stageSeedBundle(version);
}
