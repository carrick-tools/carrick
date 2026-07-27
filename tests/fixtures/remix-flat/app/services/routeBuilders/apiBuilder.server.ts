// A route-builder factory: it owns auth, body parsing and the response
// envelope, and returns the handler the route module exports. Nothing here is
// an HTTP registration a call-site scan can see — the route's path lives in
// the *filename* of the module that calls this, and its method in the name of
// the export that holds the result.

type Schema = { parse: (input: unknown) => unknown };

type RouteOptions = {
  body?: Schema;
  params?: Schema;
};

type Handler<T> = (args: {
  body: unknown;
  params: Record<string, string>;
}) => Promise<T>;

export function makeApiRoute<T>(options: RouteOptions, handler: Handler<T>) {
  return async (request: Request) => {
    const parsed = options.body ? options.body.parse(await request.json()) : {};
    return handler({ body: parsed, params: {} });
  };
}

export function makeReadRoute<T>(options: RouteOptions, handler: Handler<T>) {
  return makeApiRoute(options, handler);
}
