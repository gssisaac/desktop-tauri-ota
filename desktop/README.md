# __APP_NAME__ Desktop (Tauri launcher)

Tauri v2 shell. Release builds embed the current full-stack bundle (`resources/seed-bundle/`, version file plus zip) so the app opens offline. After the window is up, the shell checks for a newer signed bundle from `https://bundles.__APP_DOMAIN__` and downloads it in the background. The running copy stays on `127.0.0.1:__APP_PORT_PROD__` until the user refreshes.

## Layout

| Path | Role |
|------|------|
| `launcher/` | Splash UI while the shipped or installed bundle starts |
| `updater/UpdateReadyNotice.tsx` | Bottom-left notice to copy into the Next.js app |
| `src-tauri/resources/seed-bundle/` | App bundle copied into the DMG / NSIS installer (`version` + `bundle.zip`, generated, not committed) |
| `src-tauri/src/bundle_manager.rs` | Seed install, background OTA, verify, extract |
| `src-tauri/src/api_bridge.rs` | Bundled Node sidecar, port ownership, health wait |
| `src-tauri/src/startup.rs` | Boot sequence + webview navigation |
| `src-tauri/src/crypto.rs` | Ed25519 public key (must match release signing key) |

## Dev

From repo root (with `app/` populated):

```bash
corepack enable
pnpm --dir desktop install
pnpm dev:desktop
```

Dev uses `devUrl` port `__APP_PORT_DEV__` so an installed release app can keep `__APP_PORT_PROD__`.

## Release

Run from repo root — see [`docs/OTA_DEPLOYMENT.md`](../docs/OTA_DEPLOYMENT.md).

```bash
pnpm release:ota 1.0.0      # full-stack zip + manifest → R2
pnpm release:desktop        # signed DMG, with that bundle inside → R2 (macOS)
pnpm release:desktop:windows  # NSIS installer, with that bundle inside → R2 (Windows x64)
```

`pnpm build:desktop` / `pnpm release:desktop` run `scripts/stage-seed-bundle.mjs` first. It reuses `.ota-staging` when that zip is still newer than `app/`. Otherwise it builds with `pnpm release:ota <version> --seed-only`. Set `__APP_SLUG_UPPER___BUNDLE_VERSION` to pin the version stamped inside the installer. The default is the staged OTA version, or `app/package.json` when nothing is staged.

Mount `updater/UpdateReadyNotice.tsx` in the app so a downloaded update can be refreshed from the bottom-left.
