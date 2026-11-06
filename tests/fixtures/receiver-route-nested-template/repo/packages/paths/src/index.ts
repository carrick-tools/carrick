import { ACCOUNT_ROOT } from '@fx/shared';

// Route paths built from other constants: one from a constant in this
// module, one from a constant another package exports, and one from a value
// read at runtime.
export const AUTH_BASE = '/auth';
export const LOGIN_PATH = `${AUTH_BASE}/login`;
export const PROFILE_PATH = `${ACCOUNT_ROOT}/profile`;
export const STATUS_PATH = `${runtimeRoot()}/status`;

function runtimeRoot(): string {
  return String(Date.now());
}
