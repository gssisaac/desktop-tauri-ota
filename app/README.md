# `app/` (not included)

This boilerplate ships the **Tauri launcher + OTA release scripts** only. Copy a Next.js app here before running `pnpm dev:desktop` or `pnpm release:ota`.

Recommended source: [`app-next.js`](../app-next.js) from the same boilerplates repo, or your product’s existing `app/` package.

After copy, wire the app for desktop OTA (see [`AGENT.md`](../AGENT.md)):

- Standalone build flag: `__APP_SLUG_UPPER___API_STANDALONE=1` in `next.config.ts`
- User data dir env: `__APP_SLUG_UPPER___HOME` (matches the launcher’s `api_bridge.rs`)
- `GET /api/health` must return `{ data: { app: "__APP_SLUG__", bundleVersion: "…" } }`
- Mount [`desktop/updater/UpdateReadyNotice.tsx`](../desktop/updater/UpdateReadyNotice.tsx) once in the app shell, and depend on `@tauri-apps/api`. It announces a downloaded update at the bottom-left and refreshes onto it.
