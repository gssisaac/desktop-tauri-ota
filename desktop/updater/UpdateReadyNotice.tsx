'use client';

import { useEffect, useState } from 'react';

const UPDATE_READY_EVENT = '__APP_SLUG__://update-ready';

function isDesktopRuntime(): boolean {
  if (typeof window === 'undefined') return false;
  const w = window as Window & { __TAURI_INTERNALS__?: unknown; __TAURI__?: unknown };
  return Boolean(w.__TAURI_INTERNALS__ ?? w.__TAURI__);
}

/**
 * Bottom-left notice once a newer bundle has downloaded. Copy this into the Next.js app
 * shell and mount it once. The running bundle stays put until Refresh.
 *
 * Listens for `__APP_SLUG__://update-ready` and calls `pending_update` / `apply_pending_update`.
 */
export function UpdateReadyNotice() {
  const [version, setVersion] = useState<string | null>(null);
  const [refreshing, setRefreshing] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!isDesktopRuntime()) return;
    let alive = true;
    let unlisten: (() => void) | undefined;

    void (async () => {
      try {
        const { listen } = await import('@tauri-apps/api/event');
        const { invoke } = await import('@tauri-apps/api/core');
        unlisten = await listen<{ version: string }>(UPDATE_READY_EVENT, (event) => {
          if (!alive) return;
          const next = event.payload?.version;
          if (next) {
            setVersion(next);
            setError(null);
          }
        });
        const pending = await invoke<string | null>('pending_update');
        if (alive && pending) setVersion(pending);
      } catch {
        // Browser dev, or the shell has not registered the command yet.
      }
    })();

    return () => {
      alive = false;
      unlisten?.();
    };
  }, []);

  if (!version) return null;

  return (
    <div
      role="status"
      aria-live="polite"
      style={{
        position: 'fixed',
        bottom: 12,
        left: 12,
        zIndex: 20,
        width: 300,
        padding: 12,
        border: '1px solid rgba(127,127,127,0.35)',
        background: 'Canvas',
        color: 'CanvasText',
        boxShadow: '0 8px 24px rgba(0,0,0,0.18)',
      }}
    >
      <p style={{ margin: 0, fontSize: 12, fontWeight: 600 }}>Update downloaded</p>
      <p style={{ margin: '4px 0 0', fontSize: 11, opacity: 0.75 }}>
        __APP_NAME__ {version} is ready. Refresh to start using it.
      </p>
      {error ? <p style={{ margin: '8px 0 0', fontSize: 11, color: '#b91c1c' }}>{error}</p> : null}
      <div style={{ marginTop: 10, display: 'flex', justifyContent: 'flex-end' }}>
        <button
          type="button"
          disabled={refreshing}
          onClick={() => {
            setRefreshing(true);
            setError(null);
            void (async () => {
              try {
                const { invoke } = await import('@tauri-apps/api/core');
                await invoke('apply_pending_update');
              } catch (err) {
                setRefreshing(false);
                setError(err instanceof Error ? err.message : 'Could not refresh. Try again.');
              }
            })();
          }}
        >
          {refreshing ? 'Refreshing…' : 'Refresh'}
        </button>
      </div>
    </div>
  );
}
