/**
 * The environment for a package tool the scan spawns (Deno, the check's
 * package manager).
 *
 * Such a tool needs PATH, HOME, its cache and proxy settings, and whatever
 * registry auth the user configured for it (for example NPM_TOKEN). It never
 * needs Carrick's own credentials or the CI runner's tokens, and it can read
 * config the scanned tree supplies, so those keys are left out. Every other
 * key passes through unchanged.
 */

/** Exact keys a package tool never receives. */
const DROPPED_KEYS = new Set(['GITHUB_TOKEN', 'GH_TOKEN']);

/** Key prefixes a package tool never receives. */
const DROPPED_PREFIXES = ['CARRICK_', 'ACTIONS_', 'AWS_'];

export function isDroppedToolEnvKey(key: string): boolean {
  const upper = key.toUpperCase();
  return DROPPED_KEYS.has(upper) || DROPPED_PREFIXES.some(prefix => upper.startsWith(prefix));
}

export function toolEnv(
  extra: NodeJS.ProcessEnv = {},
  source: NodeJS.ProcessEnv = process.env
): NodeJS.ProcessEnv {
  const env: NodeJS.ProcessEnv = {};
  for (const [key, value] of Object.entries(source)) {
    if (value !== undefined && !isDroppedToolEnvKey(key)) env[key] = value;
  }
  return { ...env, ...extra };
}
