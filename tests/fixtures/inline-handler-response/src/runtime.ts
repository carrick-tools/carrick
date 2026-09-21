// Minimal local stand-in for an HTTP framework whose handler sends its payload
// through a context object and returns the platform `Response`. Self-contained
// on purpose: no package to install, nothing to resolve outside this directory.
// The shape — `router.get(path, handler)` with the handler passed inline, and
// `ctx.json(payload)` as the send — is the one this fixture is about; the names
// are this file's own.

export interface RequestReader {
  param(name: string): string;
  json<T>(): Promise<T>;
}

export interface Context {
  req: RequestReader;
  /** Sends `body` as the response payload. Returns the transport wrapper. */
  json<T>(body: T, status?: number): Response;
  /** Sends nothing but a status. */
  status(code: number): Response;
}

/// A handler may return the transport its send produced, or nothing at all —
/// the send has already written the response either way.
export type Handler = (
  ctx: Context,
) => Promise<Response | void> | Response | void;

/** A gate placed in front of a handler. It sends a body of its own on refusal. */
export function requireAuth(): Handler {
  return (ctx) => ctx.json({ error: 'not authorised' }, 401);
}

export class Router {
  get(path: string, ...handlers: Handler[]): this {
    void path;
    void handlers;
    return this;
  }

  post(path: string, ...handlers: Handler[]): this {
    void path;
    void handlers;
    return this;
  }
}
