# mac-app-tauri — Thin launcher + full-stack OTA

Production pattern from TablePad: the **desktop binary is only a launcher** (Tauri + bundled Node.js 22). The real app (Next.js standalone server: UI + API) ships as **Ed25519-signed OTA zips** on Cloudflare R2.

## What you get

```
mac-app-tauri/          ← copy to productions/{slug}/ (merge with app/)
  app/                  ← placeholder; copy from app-next.js
  desktop/              ← Tauri v2 launcher + splash UI
  scripts/              ← OTA build, DMG/NSIS release, R2 upload
  docs/OTA_DEPLOYMENT.md
```

## Placeholders

Replace every `__APP_*__` token before building (see [`AGENT.md`](AGENT.md)).

| Token | Example |
|-------|---------|
| `__APP_NAME__` | Reola |
| `__APP_SLUG__` | reola |
| `__APP_SLUG_UPPER__` | REOLA |
| `__APP_SLUG_SNAKE__` | reola_desktop |
| `__APP_IDENTIFIER__` | com.reola.desktop |
| `__APP_DOMAIN__` | reola.dev |
| `__APP_PORT_PROD__` | 32834 |
| `__APP_PORT_DEV__` | 32835 |

## Quick start (after scaffolding)

```bash
corepack enable
pnpm --dir desktop install
pnpm dev:desktop
```

Release commands run from this folder’s root `package.json` once `app/` exists and Cloudflare R2 is configured.
