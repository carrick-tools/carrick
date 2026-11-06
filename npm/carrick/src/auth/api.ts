// The one client for the check-or-upload endpoint: a bearer POST of one action.
//
// Lifted out of `init/projects.ts`, where it was private, so the prompt hook
// (`hook/lookup.ts`) posts through the same code rather than a second client.

import { API_BASE } from "./credentials.ts";

/** How long any one request may take when the caller sets no tighter bound. */
const DEFAULT_TIMEOUT_MS = 30_000;

/**
 * POST one action, or null when the request never reached the server.
 *
 * `signal` is the caller's own bound (a hook's deadline); the default timeout
 * applies as well, whichever fires first. `headers` is added to the bearer and
 * content-type headers.
 */
export async function post(
  token: string,
  body: Record<string, unknown>,
  request: typeof fetch = fetch,
  signal?: AbortSignal,
  headers: Record<string, string> = {},
): Promise<Response | null> {
  try {
    return await request(`${API_BASE}/types/check-or-upload`, {
      method: "POST",
      redirect: "error",
      signal: AbortSignal.any([AbortSignal.timeout(DEFAULT_TIMEOUT_MS), ...(signal ? [signal] : [])]),
      headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json", ...headers },
      body: JSON.stringify(body),
    });
  } catch {
    // A request that never reached the server says nothing about whether the
    // action exists, and the workspace read that precedes this one already
    // reports an unreachable API. Fall back rather than fail the run.
    return null;
  }
}
