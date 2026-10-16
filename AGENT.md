# AGENT.md — mac-app-tauri (launcher + OTA)

Instructions for scaffolding a **macOS/Windows desktop app** where Tauri is a thin launcher and the product ships as signed **full-stack OTA bundles** (Next.js standalone).

## When to use

- Native desktop shell with **frequent app updates without reinstalling** the DMG/installer
- Next.js app with API routes and optional native modules (e.g. `better-sqlite3`)
- Distribution via **Cloudflare R2** (`bundles.__APP_DOMAIN__`)

The installer ships the current bundle as a resource so the first launch works offline. Later versions still arrive as signed OTA zips; the running copy stays up until the user refreshes.

## Target repo layout

Copy this folder into the product repo so `app/`, `desktop/`, and `scripts/` are siblings:

```
productions/{slug}/
  app/           ← from app-next.js (or existing app)
  desktop/       ← from mac-app-tauri/desktop
  scripts/       ← from mac-app-tauri/scripts (or merge with root scripts/)
  package.json   ← merge release scripts from mac-app-tauri/package.json
  docs/          ← optional: copy docs/OTA_DEPLOYMENT.md
```

## Required inputs

| Input | Token | Notes |
|-------|--------|--------|
| Display name | `__APP_NAME__` | Window title, DMG name |
| Kebab slug | `__APP_SLUG__` | Zip names, health `app` field |
| Upper slug | `__APP_SLUG_UPPER__` | Env vars: `{UPPER}_HOME`, `{UPPER}_R2_BUCKET`, … |
| Snake slug | `__APP_SLUG_SNAKE__` | Rust crate (`reola_desktop`), no hyphens |
| Bundle ID | `__APP_IDENTIFIER__` | Tauri `identifier`, app data subfolder |
| Apex domain | `__APP_DOMAIN__` | R2 custom domain `bundles.{domain}` |
| Prod port | `__APP_PORT_PROD__` | **Numeric** — launcher server + capabilities URL |
| Dev port | `__APP_PORT_DEV__` | **Numeric** — `tauri.conf.json` `devUrl` only |

Pick ports that do not collide with other local apps.

## Copy procedure

1. Copy `mac-app-tauri/desktop`, `mac-app-tauri/scripts`, and root `package.json` scripts into the product repo.
2. Copy `app-next.js` into `app/` (or use existing `app/`).
3. **Do not copy** `node_modules`, `target`, `binaries`, `.ota`, staging dirs, or secrets.
4. Search-replace **project-wide** all `__APP_*__` tokens (including `scripts/app-config.mjs`).
5. Replace `__APP_PORT_PROD__` in Rust with a **numeric literal** (e.g. `32834`) in `api_bridge.rs` — placeholders are not valid Rust syntax until replaced.
6. Replace icons under `desktop/src-tauri/icons/` and `desktop/launcher/icon.png`.
7. Run `corepack enable && pnpm --dir desktop install`.

## Wire the Next.js `app/` package

### Standalone / OTA build

In `app/next.config.ts`, when `process.env.{__APP_SLUG_UPPER___API_STANDALONE}` is set:

- `output: 'standalone'`
- `distDir: '.next-standalone'` (or match `scripts/release-ota-bundle.mjs`)

### Runtime env (launcher sets these)

| Env | Purpose |
|-----|---------|
| `{__APP_SLUG_UPPER___HOME}` | SQLite/user data (never inside the OTA zip) |
| `{__APP_SLUG_UPPER___BUNDLE_VERSION}` | Health check version match |

### Health endpoint

`GET /api/health` must return JSON like:

```json
{ "data": { "app": "__APP_SLUG__", "bundleVersion": "1.0.0" } }
```

The launcher filters on `app === "__APP_SLUG__"` and waits for matching `bundleVersion`.

## Release workflow

1. **First time:** provision R2 + signing — [`docs/OTA_DEPLOYMENT.md`](docs/OTA_DEPLOYMENT.md).
2. `pnpm release:ota 1.0.0` — builds `app/`, smoke-tests on bundled Node 22, signs, uploads `bundles/{slug}-{version}.zip` + `manifest.json`.
3. `pnpm release:desktop` — signed notarized DMG. `desktop` prebuild seeds that bundle into the installer (`resources/seed-bundle/`, version file + zip). Sanity cap is 400 MB.
4. `pnpm release:desktop:windows` — NSIS on Windows x64, same seed (optional signing later).

`pnpm release:ota <version> --seed-only` writes the installer seed without uploading. Set `{__APP_SLUG_UPPER___BUNDLE_VERSION}` to pin the seeded version; otherwise the staged OTA manifest or `app/package.json` version is used.

Publish an OTA when installed apps should pick up a newer bundle without a new installer. A DMG or NSIS build already contains the current bundle.

## In-app update notice

After the window is open the launcher downloads a newer bundle in the background and emits `__APP_SLUG__://update-ready`. It does not switch until the user refreshes.

Copy [`desktop/updater/UpdateReadyNotice.tsx`](desktop/updater/UpdateReadyNotice.tsx) into the Next.js app shell and mount it once. It calls `pending_update` on load and `apply_pending_update` from the Refresh button. The capability already allows `http://127.0.0.1:__APP_PORT_PROD__`. Add `@tauri-apps/api` to the app.

## Customization order

1. `scripts/app-config.mjs` — central tokens (or rely on global replace)
2. `desktop/src-tauri/tauri.conf.json` — version, window, identifier
3. `desktop/src-tauri/src/crypto.rs` — `OTA_PUBLIC_KEY_HEX` (auto-synced on first `release:ota`)
4. `desktop/launcher/index.html` — branding
5. `app/src/lib/theme/brand.ts` — in-app brand

## Pitfalls

- **Node ABI:** OTA release must run on Node 22 (see `scripts/prepare-node-sidecar.mjs` `NODE_VERSION`).
- **Secrets in zip:** `release-ota` aborts if `.env*` or DB files remain in the bundle.
- **Port collision:** Launcher frees `__APP_PORT_PROD__` on startup (kills prior listener).
- **Invalid JS until replace:** Do not run release scripts until `__APP_*__` tokens are replaced.
- **Rust crate names:** Use `reola_desktop`, not `reola-desktop`.
- **Windows vs macOS OTA:** Separate manifests (`manifest.json` vs `manifest-windows-x64.json`) and signing keys.
- **Nested seed zip:** Notarization unpacks `resources/seed-bundle/bundle.zip`. `release-ota` Developer ID-signs `.node`, `.dylib`, and `spawn-helper` before zipping when a signing identity is available.

## Verify

```bash
# After placeholder replacement
pnpm --dir desktop dev
pnpm release:ota 0.0.1 --dry-run   # needs app/ built for standalone
pnpm build:desktop               # local DMG only
```
