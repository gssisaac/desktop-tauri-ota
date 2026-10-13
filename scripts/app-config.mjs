/**
 * Product identity tokens — replace every __APP_*__ when scaffolding from this boilerplate.
 * Scripts read env vars as `${APP_SLUG_UPPER}_SUFFIX` after replacement.
 */
export const APP_NAME = '__APP_NAME__';
export const APP_SLUG = '__APP_SLUG__';
export const APP_SLUG_UPPER = '__APP_SLUG_UPPER__';
export const APP_IDENTIFIER = '__APP_IDENTIFIER__';
export const APP_DOMAIN = '__APP_DOMAIN__';
export const APP_PORT_PROD = '__APP_PORT_PROD__';
export const APP_PORT_DEV = '__APP_PORT_DEV__';

/** @param {string} suffix e.g. `R2_BUCKET` */
export function envVar(suffix) {
  return `${APP_SLUG_UPPER}_${suffix}`;
}
