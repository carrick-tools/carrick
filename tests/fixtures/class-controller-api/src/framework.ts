// Minimal local stand-in for a class-controller HTTP framework, so the fixture
// is self-contained: no package to install, nothing to resolve outside this
// directory. The scanner never reads this file for meaning — it derives the
// routes structurally from `routes.ts` and the controller classes — but the
// fixture has to parse and type-check as ordinary TypeScript.

export interface Request {
  body: unknown;
  /** Validates the body against a JSON schema, named by its `$id` URL. */
  validate(schemaId: string): void;
}

export interface Context {
  request: Request;
  response: {
    body: unknown;
    status: number;
    type: string;
  };
  params: Record<string, string>;
}

export type Middleware = (ctx: Context, next: () => Promise<void>) => Promise<void>;

/** Base class every controller in this service extends. */
export class Controller {
  dispatch(ctx: Context): Promise<void> {
    void ctx;
    return Promise.resolve();
  }
}

/** Anything a route can be bound to: a controller instance or a middleware. */
export type Handler = Controller | Middleware;

/**
 * Binds a path to a controller, optionally behind middleware:
 * `router('/widget', widget)` or `router('/token', errorHandler, token)`.
 */
export function router(path: string, ...handlers: Handler[]): Middleware {
  void path;
  void handlers;
  return async (_ctx, next) => next();
}

/** Declares the HTTP method a non-verb-named handler answers. */
export function method(verb: string): MethodDecorator {
  return (_target, _key, descriptor) => {
    void verb;
    return descriptor;
  };
}

/** Declares the content type a handler emits. Not a route fact. */
export function accept(contentType: string): MethodDecorator {
  return (_target, _key, descriptor) => {
    void contentType;
    return descriptor;
  };
}
