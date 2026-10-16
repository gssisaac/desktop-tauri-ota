# OTA + desktop launcher deployment

Neutral guide for **full-stack OTA bundles** and **thin desktop launcher** releases on Cloudflare R2.

## Architecture

```
┌─────────────────────────────┬──────────────────────────────────────┐
│  Desktop launcher (DMG/EXE) │  OTA bundle (zip on R2)              │
├─────────────────────────────┼──────────────────────────────────────┤
│ Tauri + splash UI           │ Next.js standalone (server.js)       │
│ Bundled Node 22 sidecar     │ UI + /api/* on 127.0.0.1:{prod port} │
│ Current bundle seeded in    │ Ed25519 + SHA-256 verified on install│
│ the DMG / NSIS installer    │                                       │
└─────────────────────────────┴──────────────────────────────────────┘
```

Base URL: `https://bundles.__APP_DOMAIN__`

## R2 bucket layout

```
__APP_SLUG__-bundles/
├── manifest.json                    # macOS OTA pointer
├── manifest-windows-x64.json        # Windows OTA pointer
├── bundles/
│   ├── __APP_SLUG__-1.0.0.zip
│   └── __APP_SLUG__-1.0.0-win-x64.zip
└── downloads/
    ├── latest.json                  # macOS DMG pointer
    ├── latest-windows.json          # Windows installer pointer
    ├── __APP_NAME___1.0.0_aarch64.dmg
    └── __APP_NAME___1.0.0_x64-setup.exe
```

Cache hints:

- `manifest*.json`, `latest*.json` → `Cache-Control: no-cache`
- `bundles/*.zip`, `downloads/*` → `public, max-age=31536000, immutable` (version in filename)

## Cloudflare setup

1. Create R2 bucket `__APP_SLUG__-bundles`.
2. Attach custom domain `bundles.__APP_DOMAIN__` to the bucket.
3. Optional: upload `health.json` for connectivity checks.

Wrangler (example — set `CLOUDFLARE_ACCOUNT_ID` in env):

```bash
pnpm dlx wrangler@4 login
pnpm dlx wrangler@4 r2 bucket create __APP_SLUG__-bundles
# Custom domain: R2 dashboard or `wrangler r2 bucket domain add`
```

## Environment variables

Loaded from repo `.env`, `.env.local`, and gitignored `.ota/credentials.env` (macOS) or `.ota/credentials-windows.env` (Windows).

| Variable | Purpose |
|----------|---------|
| `CLOUDFLARE_API_TOKEN` | R2 write (optional if `wrangler login`) |
| `CLOUDFLARE_ACCOUNT_ID` | Cloudflare account |
| `{__APP_SLUG_UPPER___R2_BUCKET}` | Override bucket name |
| `{__APP_SLUG_UPPER___BUNDLE_BASE}` | Override bundle base URL |
| `{__APP_SLUG_UPPER___OTA_PRIVATE_KEY_HEX}` | Ed25519 signing seed (64 hex chars) |
| `{__APP_SLUG_UPPER___MIN_APP_VERSION}` | Oldest launcher allowed to install bundle |

On first `pnpm release:ota`, a keypair may be generated and `desktop/src-tauri/src/crypto.rs` updated — **rebuild the launcher** after key changes.

Generate keys manually:

```bash
node -e "const {generateKeyPairSync}=require('crypto');const {privateKey,publicKey}=generateKeyPairSync('ed25519');const sk=privateKey.export({type:'pkcs8',format:'der'}).slice(-32);const pk=publicKey.export({type:'spki',format:'der'}).slice(-32);console.log('PRIVATE='+sk.toString('hex'));console.log('PUBLIC='+pk.toString('hex'));"
```

## Release commands

From product repo root (with `app/` present):

```bash
pnpm release:ota 1.0.0              # build, smoke-test, sign, upload
pnpm release:ota 1.0.0 --dry-run      # artifacts in .ota-staging/
pnpm release:desktop                  # macOS DMG + latest.json
pnpm release:desktop --dry-run
pnpm release:desktop:windows          # Windows x64 NSIS (run on Windows)
```

### macOS signing & notarization

1. Copy `scripts/deploy/apple-signing.env.example` → `scripts/deploy/apple-signing.env` (gitignored).
2. Fill Developer ID Application identity + App Store Connect API key (recommended).
3. `release:desktop` runs Tauri build, notarizes `.app`, then `scripts/deploy/notarize-dmg.sh` staples the DMG.
4. Upload is blocked unless Gatekeeper reports **Notarized Developer ID**.

## Client behavior (launcher)

1. Launch the installed bundle under Application Support. On first launch, unpack the bundle shipped inside the DMG or NSIS installer (`resources/seed-bundle/version`).
2. Open that version immediately. A download does not block startup unless nothing local exists.
3. After the window is up, fetch `manifest.json` (or the Windows variant). If it is newer and the launcher meets `minAppVersion`, download and verify in the background.
4. Leave the new files pending. The running server stays on the current version.
5. The app shows a bottom-left notice with Refresh (`desktop/updater/UpdateReadyNotice.tsx`). Refresh restarts onto the pending version.
6. Offline: keep running the installed or shipped bundle.

## Checklist

| Step | Command / action |
|------|------------------|
| Replace all `__APP_*__` tokens | project-wide search |
| Wire `app/` standalone + health | AGENT.md |
| R2 bucket + domain | Cloudflare |
| First OTA | `pnpm release:ota <version>` |
| Launcher build | `pnpm release:desktop` |
| Verify manifest | `curl -sS https://bundles.__APP_DOMAIN__/manifest.json` |
