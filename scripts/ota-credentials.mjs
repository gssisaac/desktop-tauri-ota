/**
 * OTA signing credentials: load, validate against crypto.rs, bootstrap into .ota/credentials.env
 * (.ota/credentials-windows.env on Windows: Windows bundles have their own key and constant).
 */
import { createPrivateKey, createPublicKey, generateKeyPairSync } from 'node:crypto';
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { envVar } from './app-config.mjs';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const isWindows = process.platform === 'win32';
const credentialsFile = isWindows ? 'credentials-windows.env' : 'credentials.env';
const credentialsPath = join(root, '.ota', credentialsFile);
const cryptoRsPath = join(root, 'desktop', 'src-tauri', 'src', 'crypto.rs');
const PRIVATE_KEY_VAR = isWindows
  ? envVar('OTA_WINDOWS_PRIVATE_KEY_HEX')
  : envVar('OTA_PRIVATE_KEY_HEX');
const PUBLIC_KEY_VAR = isWindows
  ? envVar('OTA_WINDOWS_PUBLIC_KEY_HEX')
  : envVar('OTA_PUBLIC_KEY_HEX');
/** The crypto.rs `OTA_PUBLIC_KEY_HEX` constant for this platform's `#[cfg(...)]`. */
const EMBEDDED_KEY = new RegExp(
  String.raw`(#\[cfg\(${isWindows ? 'windows' : String.raw`not\(windows\)`}\)\]\s*pub const OTA_PUBLIC_KEY_HEX:\s*&str\s*=\s*\n\s*")([0-9a-fA-F]{64})(")`,
);

export function parseEnvFile(filePath) {
  /** @type {Record<string, string>} */
  const out = {};
  for (const line of readFileSync(filePath, 'utf8').split('\n')) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith('#')) continue;
    const eq = trimmed.indexOf('=');
    if (eq === -1) continue;
    const key = trimmed.slice(0, eq).trim();
    let value = trimmed.slice(eq + 1).trim();
    if (
      (value.startsWith('"') && value.endsWith('"')) ||
      (value.startsWith("'") && value.endsWith("'"))
    ) {
      value = value.slice(1, -1);
    }
    out[key] = value;
  }
  return out;
}

export function loadMergedEnvIntoProcess() {
  const merged = {};
  const envFiles = [
    join(root, '.env'),
    join(root, '.env.local'),
    join(root, 'app', '.env'),
    join(root, 'app', '.env.local'),
    credentialsPath,
  ];
  for (const filePath of envFiles) {
    if (existsSync(filePath)) {
      Object.assign(merged, parseEnvFile(filePath));
    }
  }
  for (const [key, value] of Object.entries(merged)) {
    if (!(key in process.env)) {
      process.env[key] = value;
    }
  }
}

export function readEmbeddedPublicKeyHex() {
  const source = readFileSync(cryptoRsPath, 'utf8');
  const match = source.match(EMBEDDED_KEY);
  if (!match) {
    throw new Error(`Could not parse OTA_PUBLIC_KEY_HEX in ${cryptoRsPath}`);
  }
  return match[2].toLowerCase();
}

export function publicKeyHexFromSeed(seedHex) {
  const seed = Buffer.from(seedHex, 'hex');
  if (seed.length !== 32) {
    throw new Error('OTA private seed must be 32 bytes (64 hex chars)');
  }
  const derPrefix = Buffer.from('302e020100300506032b657004220420', 'hex');
  const pkcs8 = Buffer.concat([derPrefix, seed]);
  const privateKey = createPrivateKey({ key: pkcs8, format: 'der', type: 'pkcs8' });
  const publicKey = createPublicKey(privateKey);
  const spki = publicKey.export({ format: 'der', type: 'spki' });
  return Buffer.from(spki).subarray(-32).toString('hex');
}

function writeCredentialsFile(privateKeyHex, publicKeyHex) {
  mkdirSync(dirname(credentialsPath), { recursive: true });
  const body = `# Auto-managed by pnpm release:ota — do not commit (gitignored)
${PRIVATE_KEY_VAR}=${privateKeyHex}
${PUBLIC_KEY_VAR}=${publicKeyHex}
`;
  writeFileSync(credentialsPath, body, { mode: 0o600 });
}

export function writeEmbeddedPublicKeyHex(publicKeyHex) {
  const normalized = publicKeyHex.toLowerCase();
  let source = readFileSync(cryptoRsPath, 'utf8');
  const next = source.replace(EMBEDDED_KEY, `$1${normalized}$3`);
  if (next === source) {
    throw new Error('Failed to update OTA_PUBLIC_KEY_HEX in crypto.rs');
  }
  writeFileSync(cryptoRsPath, next, 'utf8');
}

function generateKeyPairHex() {
  const { privateKey, publicKey } = generateKeyPairSync('ed25519');
  const seed = privateKey.export({ type: 'pkcs8', format: 'der' }).subarray(-32);
  const pub = publicKey.export({ type: 'spki', format: 'der' }).subarray(-32);
  return {
    privateKeyHex: seed.toString('hex'),
    publicKeyHex: pub.toString('hex'),
  };
}

/**
 * Returns 64-char hex Ed25519 seed. Creates .ota/credentials.env (credentials-windows.env on
 * Windows) on first use when needed.
 */
export function ensureOtaPrivateKeyHex() {
  loadMergedEnvIntoProcess();

  let privateKeyHex = process.env[PRIVATE_KEY_VAR]?.trim().toLowerCase();
  const embeddedPublic = readEmbeddedPublicKeyHex();

  if (privateKeyHex) {
    const derivedPublic = publicKeyHexFromSeed(privateKeyHex);
    if (derivedPublic !== embeddedPublic) {
      console.warn(
        '[release-ota] env private key does not match crypto.rs — updating embedded public key',
      );
      writeEmbeddedPublicKeyHex(derivedPublic);
    }
    if (!existsSync(credentialsPath)) {
      writeCredentialsFile(privateKeyHex, derivedPublic);
      console.log(`[release-ota] saved signing key to .ota/${credentialsFile}`);
    }
    return privateKeyHex;
  }

  if (existsSync(credentialsPath)) {
    const fromFile = parseEnvFile(credentialsPath)[PRIVATE_KEY_VAR]?.trim().toLowerCase();
    if (fromFile) {
      privateKeyHex = fromFile;
      process.env[PRIVATE_KEY_VAR] = privateKeyHex;
      const derivedPublic = publicKeyHexFromSeed(privateKeyHex);
      if (derivedPublic !== embeddedPublic) {
        throw new Error(
          `\`.ota/${credentialsFile}\` does not match desktop/src-tauri/src/crypto.rs. ` +
            `Restore the correct key or delete .ota/${credentialsFile} to regenerate (requires desktop rebuild).`,
        );
      }
      return privateKeyHex;
    }
  }

  console.log('[release-ota] no signing key found — generating Ed25519 keypair…');
  const { privateKeyHex: generatedPrivate, publicKeyHex: generatedPublic } = generateKeyPairHex();

  if (generatedPublic !== embeddedPublic) {
    console.warn(
      '[release-ota] updating OTA_PUBLIC_KEY_HEX in crypto.rs (rebuild __APP_NAME__ desktop for OTA verify)',
    );
    writeEmbeddedPublicKeyHex(generatedPublic);
  }

  writeCredentialsFile(generatedPrivate, generatedPublic);
  process.env[PRIVATE_KEY_VAR] = generatedPrivate;
  console.log(`[release-ota] wrote .ota/${credentialsFile}`);
  return generatedPrivate;
}
