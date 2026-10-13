# __APP_NAME__ Desktop (Tauri launcher)

Thin Tauri v2 shell. **Release builds do not embed your UI.** They download a signed full-stack Next.js standalone bundle from `https://bundles.__APP_DOMAIN__` and run `node server.js` on `127.0.0.1:__APP_PORT_PROD__`.

## Layout

| Path | Role |
|------|------|
| `launcher/` | Splash UI (download progress, errors, retry) |
| `src-tauri/src/bundle_manager.rs` | OTA manifest, verify, extract |
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
pnpm release:desktop        # signed DMG → R2 (macOS)
pnpm release:desktop:windows  # NSIS installer → R2 (Windows x64)
```

Ship at least one OTA bundle before distributing a new launcher.
