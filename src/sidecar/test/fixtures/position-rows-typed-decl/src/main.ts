// A call through a value whose TYPE is another function's (`typeof fetch`):
// the callee symbol is the parameter, the type's own symbol is `fetch`.
interface Connection {
  fetchImpl?: typeof fetch;
}

type Handler = (id: string) => void;

function local(id: string): string {
  return id;
}

export function viaOptional(connection: Connection): Promise<Response> {
  const send = connection.fetchImpl!;
  return send('/api/status');
}

export function viaParameter(send: typeof fetch): Promise<Response> {
  return send('/api/param');
}

export function viaLocal(send: typeof local): string {
  return send('x');
}

export function viaAnonymous(h: Handler): void {
  h('y');
}

export function direct(): Promise<Response> {
  return fetch('/api/direct');
}

export function ownDeclaration(): string {
  return local('z');
}
