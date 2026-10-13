/**
 * Cloudflare R2 uploads for release scripts. One bucket (`__APP_SLUG__-bundles`, served at
 * https://bundles.__APP_DOMAIN__) holds the OTA bundles and the desktop DMG downloads.
 */
import { spawnSync } from 'node:child_process';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { APP_DOMAIN, APP_SLUG, envVar } from './app-config.mjs';
import { loadMergedEnvIntoProcess } from './ota-credentials.mjs';
import { pnpmSpawn } from './pnpm.mjs';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');

loadMergedEnvIntoProcess();

export const r2Bucket = process.env[envVar('R2_BUCKET')] || `${APP_SLUG}-bundles`;
export const r2BaseUrl = (
  process.env[envVar('BUNDLE_BASE')] || `https://bundles.${APP_DOMAIN}`
).replace(/\/$/, '');

const wranglerEnv = {
  ...process.env,
  CLOUDFLARE_ACCOUNT_ID: process.env.CLOUDFLARE_ACCOUNT_ID,
};

let warnedAboutAuth = false;

/** Upload one object to the bucket; returns whether wrangler succeeded. */
export function r2Put(key, file, contentType, cacheControl, { contentDisposition } = {}) {
  if (!warnedAboutAuth && !process.env.CLOUDFLARE_API_TOKEN?.trim()) {
    console.warn('[r2] CLOUDFLARE_API_TOKEN not set — using wrangler login if available');
    warnedAboutAuth = true;
  }
  const args = [
    'dlx',
    'wrangler@4',
    'r2',
    'object',
    'put',
    `${r2Bucket}/${key}`,
    '--file',
    file,
    '--content-type',
    contentType,
    '--cache-control',
    cacheControl,
    '--remote',
  ];
  if (contentDisposition) args.push('--content-disposition', contentDisposition);
  const result = spawnSync(...pnpmSpawn(args), { cwd: root, stdio: 'inherit', env: wranglerEnv });
  return result.status === 0;
}
