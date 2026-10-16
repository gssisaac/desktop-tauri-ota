#!/usr/bin/env node
/**
 * Full-stack OTA release: build the Next.js standalone server (UI + API), smoke-test it,
 * zip + sign it, and publish the bundle and manifest.json to Cloudflare R2. The desktop
 * launcher downloads the bundle and runs `server.js` on 127.0.0.1:__APP_PORT_PROD__ with its bundled
 * Node.js (scripts/prepare-node-sidecar.mjs), so native modules must target that Node's ABI.
 *
 * Run on macOS for manifest.json (universal arm64 + x86_64 bundle), or on Windows x64 for
 * manifest-windows-x64.json (read by the Windows launcher, bundle_manager.rs MANIFEST_URL).
 *
 * Usage: pnpm release:ota <version> [--dry-run] [--seed-only]
 * `--seed-only` builds and copies the zip into the desktop installer resources, and does not upload.
 * Signing key: auto from .ota/credentials.env (created on first run; not required for --seed-only)
 */
import { spawn, spawnSync } from 'node:child_process';
import { createHash, createPrivateKey, sign } from 'node:crypto';
import {
  cpSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  renameSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { dirname, join, relative } from 'node:path';
import { fileURLToPath } from 'node:url';
import { ensureOtaPrivateKeyHex, loadMergedEnvIntoProcess } from './ota-credentials.mjs';
import { pnpmSpawn } from './pnpm.mjs';
import {
  NODE_MODULE_VERSION,
  NODE_VERSION,
  hostTargetTriple,
  prepareNodeSidecar,
} from './prepare-node-sidecar.mjs';
import { envVar } from './app-config.mjs';
import { r2BaseUrl as baseUrl, r2Put } from './r2.mjs';
import { signMacBundleNativeBinaries } from './sign-mac-binaries.mjs';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const isWindows = process.platform === 'win32';

loadMergedEnvIntoProcess();

const args = process.argv.slice(2);
const dryRun = args.includes('--dry-run');
const seedOnly = args.includes('--seed-only');
const version = args.find((arg) => !arg.startsWith('--'));
if (!version) {
  console.error('Usage: pnpm release:ota <version> [--dry-run]');
  console.error('Example: pnpm release:ota 1.0.1');
  process.exit(1);
}
// Mirrors validate_version in desktop/src-tauri/src/bundle_manager.rs.
if (!/^[0-9A-Za-z.+_-]{1,64}$/.test(version)) {
  console.error(`[release-ota] invalid version "${version}" (allowed: A-Z a-z 0-9 . + _ -)`);
  process.exit(1);
}

// Native modules (better-sqlite3) are built for the Node running this release; the launcher
// runs the bundle on its bundled Node, so both must share one ABI.
if (process.versions.modules !== NODE_MODULE_VERSION) {
  console.error(
    `[release-ota] OTA release requires Node.js v22 (ABI ${NODE_MODULE_VERSION}). Current is Node ${process.version} (ABI ${process.versions.modules}).`,
  );
  process.exit(1);
}

// Oldest launcher that can run this bundle. Independent of the launcher's own version:
// a 1.0.0 shell still accepts app bundles built for the 0.1.1 launcher until an OTA
// actually needs a newer shell. Override with __APP_SLUG_UPPER___MIN_APP_VERSION when that happens.
const MIN_LAUNCHER_VERSION = '0.1.1';
const minAppVersion = process.env[envVar('MIN_APP_VERSION')] || MIN_LAUNCHER_VERSION;

const appRoot = join(root, 'app');
// Must match distDir for __APP_SLUG_UPPER___API_STANDALONE in app/next.config.ts.
const distDirName = '.next-standalone';
const standaloneSrc = join(appRoot, distDirName, 'standalone');
const staging = join(root, '.ota-staging');
const bundleDir = join(staging, 'bundle');

function fail(message) {
  console.error(`[release-ota] ${message}`);
  console.error('[release-ota] Staging artifacts:', staging);
  process.exit(1);
}

// 1. Build the standalone server.
console.log('[release-ota] building Next.js standalone server…');
// The build repoints the tracked next-env.d.ts at its own distDir; keep the dev version.
const nextEnvPath = join(appRoot, 'next-env.d.ts');
const nextEnv = existsSync(nextEnvPath) ? readFileSync(nextEnvPath, 'utf8') : null;
// Same as `build:standalone`, whose `VAR=1 cmd` syntax cmd.exe can't run.
const build = spawnSync(...pnpmSpawn(['--dir', appRoot, 'exec', 'next', 'build']), {
  cwd: root,
  stdio: 'inherit',
  env: { ...process.env, [envVar('API_STANDALONE')]: '1' },
});
if (nextEnv !== null) writeFileSync(nextEnvPath, nextEnv);
if (build.status !== 0) fail('standalone build failed');
if (!existsSync(join(standaloneSrc, 'server.js'))) fail(`missing app/${distDirName}/standalone/server.js`);

// 2. Assemble a self-contained bundle: server + traced node_modules + static assets.
rmSync(staging, { recursive: true, force: true });
mkdirSync(bundleDir, { recursive: true });
cpSync(standaloneSrc, bundleDir, { recursive: true, dereference: true });
cpSync(join(appRoot, distDirName, 'static'), join(bundleDir, distDirName, 'static'), {
  recursive: true,
  dereference: true,
});
if (existsSync(join(appRoot, 'public'))) {
  cpSync(join(appRoot, 'public'), join(bundleDir, 'public'), { recursive: true, dereference: true });
}

// 3. Next copies .env files and can trace ./data into standalone output. Never publish them:
//    the bundle is public, and user data lives in __APP_SLUG_UPPER___HOME on the desktop.
rmSync(join(bundleDir, 'data'), { recursive: true, force: true });
for (const name of readdirSync(bundleDir)) {
  if (name.startsWith('.env')) rmSync(join(bundleDir, name), { force: true });
}

const LOCAL_STATE = /^\.env(\..+)?$|\.(db|db-wal|db-shm|sqlite|sqlite3)$/i;
function findLocalState(dir, found = []) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    if (entry.name === 'node_modules') continue;
    const full = join(dir, entry.name);
    if (entry.isDirectory()) findLocalState(full, found);
    else if (LOCAL_STATE.test(entry.name)) found.push(relative(bundleDir, full));
  }
  return found;
}
const leaked = findLocalState(bundleDir);
if (leaked.length) fail(`refusing to publish local secrets/data: ${leaked.join(', ')}`);

// 4. better-sqlite3 is compiled per Node ABI and CPU arch. On macOS, make it a universal
//    (arm64 + x86_64) binary so one bundle runs on Apple Silicon and Intel launchers, then load
//    each slice on the launcher's Node. Windows bundles target x64 only.
const ARCHS = [
  { node: 'arm64', lipo: 'arm64', triple: 'aarch64-apple-darwin' },
  { node: 'x64', lipo: 'x86_64', triple: 'x86_64-apple-darwin' },
];
const sqliteDir = join(bundleDir, 'node_modules', 'better-sqlite3');
const sqliteBinary = join(sqliteDir, 'build', 'Release', 'better_sqlite3.node');

function lipoArchs(file) {
  const res = spawnSync('lipo', ['-archs', file], { encoding: 'utf8' });
  if (res.status !== 0) fail(`lipo -archs ${relative(bundleDir, file)} failed: ${res.stderr}`);
  return res.stdout.trim().split(/\s+/);
}

/** The prebuild `pnpm install` would fetch on a machine of that arch (better-sqlite3 releases). */
function fetchSqlitePrebuild(arch, work) {
  const dir = join(work, arch);
  mkdirSync(dir);
  cpSync(join(sqliteDir, 'package.json'), join(dir, 'package.json'));
  const res = spawnSync(
    process.execPath,
    [
      join(appRoot, 'node_modules', 'prebuild-install', 'bin.js'),
      ...['--platform', 'darwin', '--arch', arch, '--runtime', 'node', '--target', NODE_VERSION],
    ],
    { cwd: dir, stdio: 'inherit' },
  );
  const binary = join(dir, 'build', 'Release', 'better_sqlite3.node');
  if (res.status !== 0 || !existsSync(binary)) fail(`no better-sqlite3 prebuild for darwin-${arch}`);
  return binary;
}

if (!existsSync(sqliteBinary)) fail('bundle has no better-sqlite3 native binary');
const LOAD_SQLITE =
  "const D=require(process.argv[1]);if(new D(':memory:').prepare('select 1 as x').get().x!==1)process.exit(2)";
const hostTriple = hostTargetTriple();

if (isWindows) {
  const res = spawnSync(await prepareNodeSidecar(hostTriple), ['-e', LOAD_SQLITE, sqliteDir], {
    encoding: 'utf8',
  });
  if (res.error || res.status !== 0) {
    fail(`better-sqlite3 failed to load on Node v${NODE_VERSION}:\n${res.error?.message ?? res.stderr}`);
  }
  console.log(`[release-ota] better-sqlite3 win32-x64: loads on Node v${NODE_VERSION} (ABI ${NODE_MODULE_VERSION})`);
} else {
  const sqliteArchs = lipoArchs(sqliteBinary);
  const missingArchs = ARCHS.filter((arch) => !sqliteArchs.includes(arch.lipo));
  if (missingArchs.length) {
    console.log(`[release-ota] making better-sqlite3 universal (adding ${missingArchs.map((a) => a.lipo)})…`);
    const work = mkdtempSync(join(tmpdir(), '__APP_SLUG__-sqlite-prebuild-'));
    try {
      const slices = missingArchs.map((arch) => fetchSqlitePrebuild(arch.node, work));
      const universal = join(work, 'better_sqlite3.node');
      const lipo = spawnSync('lipo', ['-create', sqliteBinary, ...slices, '-output', universal], {
        encoding: 'utf8',
      });
      if (lipo.status !== 0) fail(`lipo -create failed: ${lipo.stderr}`);
      cpSync(universal, sqliteBinary);
    } finally {
      rmSync(work, { recursive: true, force: true });
    }
  }

  for (const arch of ARCHS) {
    if (!lipoArchs(sqliteBinary).includes(arch.lipo)) fail(`better-sqlite3 lacks ${arch.lipo}`);
    // Apple Silicon runs the x86_64 Node under Rosetta; an Intel Mac can't run the arm64 one.
    if (hostTriple === 'x86_64-apple-darwin' && arch.triple !== hostTriple) {
      console.warn(`[release-ota] better-sqlite3 ${arch.lipo}: not load-tested on this Intel Mac`);
      continue;
    }
    const node = await prepareNodeSidecar(arch.triple);
    const res = spawnSync(node, ['-e', LOAD_SQLITE, sqliteDir], { encoding: 'utf8' });
    if (res.error && arch.triple !== hostTriple) {
      console.warn(
        `[release-ota] better-sqlite3 ${arch.lipo}: not load-tested (${res.error.message}; is Rosetta installed?)`,
      );
      continue;
    }
    if (res.error || res.status !== 0) {
      fail(
        `better-sqlite3 ${arch.lipo} failed to load on Node v${NODE_VERSION}:\n${res.error?.message ?? res.stderr}`,
      );
    }
    console.log(`[release-ota] better-sqlite3 ${arch.lipo}: loads on Node v${NODE_VERSION} (ABI ${NODE_MODULE_VERSION})`);
  }
}

// Other native modules are platform packages (e.g. sharp's @img/sharp-darwin-arm64) that only
// cover the arch this bundle was built on.
function findNativeModules(dir, found = []) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) findNativeModules(full, found);
    else if (entry.name.endsWith('.node')) found.push(full);
  }
  return found;
}
for (const file of findNativeModules(join(bundleDir, 'node_modules'))) {
  if (file === sqliteBinary) continue;
  const archs = isWindows ? 'win32-x64' : lipoArchs(file).join(', ');
  console.log(`[release-ota] native module ${relative(bundleDir, file)}: ${archs}`);
}

// 5. Boot the bundle exactly like the launcher does (same Node) and hit real routes.
function freePort() {
  return new Promise((resolve, reject) => {
    const server = createServer();
    server.on('error', reject);
    server.listen(0, '127.0.0.1', () => {
      const { port } = server.address();
      server.close(() => resolve(port));
    });
  });
}

async function smokeTest() {
  const port = await freePort();
  const home = mkdtempSync(join(tmpdir(), '__APP_SLUG__-ota-smoke-'));
  const origin = `http://127.0.0.1:${port}`;
  let output = '';
  const child = spawn(await prepareNodeSidecar(hostTriple), ['server.js'], {
    cwd: bundleDir,
    env: {
      PATH: process.env.PATH,
      HOME: process.env.HOME,
      // Node on Windows needs these for sockets, crypto and os.tmpdir(); undefined ones are dropped.
      ...(isWindows && {
        SystemRoot: process.env.SystemRoot,
        USERPROFILE: process.env.USERPROFILE,
        APPDATA: process.env.APPDATA,
        LOCALAPPDATA: process.env.LOCALAPPDATA,
        TEMP: process.env.TEMP,
        TMP: process.env.TMP,
      }),
      HOSTNAME: '127.0.0.1',
      PORT: String(port),
      NODE_ENV: 'production',
      [envVar('HOME')]: home,
      [envVar('BUNDLE_VERSION')]: version,
    },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  child.stdout.on('data', (chunk) => (output += chunk));
  child.stderr.on('data', (chunk) => (output += chunk));

  try {
    const deadline = Date.now() + 60_000;
    for (;;) {
      if (child.exitCode !== null) throw new Error(`server exited with code ${child.exitCode}`);
      try {
        const health = await fetch(`${origin}/api/health`).then((res) => res.json());
        if (health?.data?.bundleVersion === version) break;
      } catch {
        // not listening yet
      }
      if (Date.now() > deadline) throw new Error('timed out waiting for /api/health');
      await new Promise((resolve) => setTimeout(resolve, 250));
    }
    for (const path of ['/', '/api/workspaces']) {
      const res = await fetch(`${origin}${path}`);
      if (!res.ok) throw new Error(`GET ${path} → HTTP ${res.status}`);
    }
    if (!existsSync(join(home, 'data', 'app.db'))) {
      throw new Error('server did not create its database under __APP_SLUG_UPPER___HOME');
    }
    if (existsSync(join(bundleDir, 'data'))) {
      throw new Error('server wrote data/ inside the bundle instead of __APP_SLUG_UPPER___HOME');
    }
  } catch (err) {
    console.error(output);
    throw err;
  } finally {
    const exited = child.exitCode !== null ? null : new Promise((resolve) => child.once('exit', resolve));
    child.kill();
    // Windows keeps app.db locked until the server process is gone.
    await exited;
    rmSync(home, { recursive: true, force: true, maxRetries: 5 });
  }
}

console.log('[release-ota] smoke-testing bundle…');
try {
  await smokeTest();
} catch (err) {
  fail(`smoke test failed: ${err instanceof Error ? err.message : err}`);
}

// 6. Sign native modules for the DMG seed zip (notarization scans inside bundle.zip).
if (!isWindows) {
  try {
    signMacBundleNativeBinaries(bundleDir);
  } catch (err) {
    fail(err instanceof Error ? err.message : err);
  }
}

// 7. Zip. The archive name is the version; a given name is one file.
const tmpZip = join(staging, 'bundle.zip');
// Windows has no `zip`; its bundled bsdtar writes deflate zips (`-a` picks the format from .zip).
const zipResult = isWindows
  ? spawnSync(
      join(process.env.SystemRoot ?? 'C:\\Windows', 'System32', 'tar.exe'),
      ['-a', '-c', '-f', tmpZip, ...readdirSync(bundleDir)],
      { cwd: bundleDir, stdio: 'inherit' },
    )
  : spawnSync('zip', ['-r', '-q', '-X', tmpZip, '.'], {
      cwd: bundleDir,
      stdio: 'inherit',
    });
if (zipResult.status !== 0) fail('zip failed');

const zipBytes = readFileSync(tmpZip);
const sha256 = createHash('sha256').update(zipBytes).digest('hex');
const zipName = `__APP_SLUG__-${version}${isWindows ? '-win-x64' : ''}.zip`;
const manifestKey = isWindows ? 'manifest-windows-x64.json' : 'manifest.json';
const zipPath = join(staging, zipName);
renameSync(tmpZip, zipPath);

const seedDir = join(root, 'desktop', 'src-tauri', 'resources', 'seed-bundle');
mkdirSync(seedDir, { recursive: true });
cpSync(zipPath, join(seedDir, 'bundle.zip'));
writeFileSync(join(seedDir, 'version'), `${version}\n`);
console.log(`[release-ota] installer seed v${version} → ${seedDir}`);

if (seedOnly) {
  const localManifest = {
    version,
    minAppVersion,
    size: zipBytes.length,
    releaseDate: new Date().toISOString(),
  };
  writeFileSync(join(staging, manifestKey), `${JSON.stringify(localManifest, null, 2)}\n`);
  console.log(`[release-ota] bundle size: ${(zipBytes.length / 1_048_576).toFixed(1)} MB`);
  console.log('[release-ota] --seed-only: skipping publish');
  process.exit(0);
}

let privateKeyHex;
try {
  privateKeyHex = ensureOtaPrivateKeyHex();
} catch (err) {
  fail(err instanceof Error ? err.message : err);
}

const seed = Buffer.from(privateKeyHex, 'hex');
if (seed.length !== 32) fail('the OTA private key must be 32 bytes (64 hex chars)');

const derPrefix = Buffer.from('302e020100300506032b657004220420', 'hex');
const pkcs8 = Buffer.concat([derPrefix, seed]);
const privateKey = createPrivateKey({ key: pkcs8, format: 'der', type: 'pkcs8' });
const signature = sign(null, zipBytes, privateKey).toString('hex');

const manifest = {
  version,
  minAppVersion,
  url: `${baseUrl}/bundles/${zipName}`,
  sha256,
  signature,
  size: zipBytes.length,
  releaseDate: new Date().toISOString(),
};

const manifestPath = join(staging, manifestKey);
writeFileSync(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`);

console.log('[release-ota] manifest:', manifest);
console.log(`[release-ota] bundle size: ${(zipBytes.length / 1_048_576).toFixed(1)} MB`);

if (dryRun) {
  console.log('[release-ota] --dry-run: skipping upload. Artifacts:', staging);
  process.exit(0);
}

// 8. Upload the bundle before the manifest so clients never see a URL that 404s.
if (!r2Put(`bundles/${zipName}`, zipPath, 'application/zip', 'public, max-age=31536000, immutable')) {
  fail('bundle upload failed. Fix auth (.env or wrangler login) and retry.');
}
if (!r2Put(manifestKey, manifestPath, 'application/json', 'no-cache')) {
  fail('manifest upload failed after the bundle was uploaded; rerun to publish it.');
}

console.log('[release-ota] done →', `${baseUrl}/${manifestKey}`);
