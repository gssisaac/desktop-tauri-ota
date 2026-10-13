/**
 * `spawnSync(...pnpmSpawn(args), options)`: runs pnpm without a shell on every platform. On
 * Windows `pnpm` is a `.cmd` shim, which Node refuses to spawn without `shell: true`, and a
 * shell would re-parse arguments such as `attachment; filename="…"`.
 */
export function pnpmSpawn(args) {
  const entry = process.env.npm_execpath;
  if (entry && /pnpm/i.test(entry)) return [process.execPath, [entry, ...args]];
  if (process.platform === 'win32') {
    throw new Error('on Windows, run release scripts through pnpm (e.g. `pnpm release:ota 1.0.0`)');
  }
  return ['pnpm', args];
}
