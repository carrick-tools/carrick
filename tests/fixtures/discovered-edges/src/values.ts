// A parameter callee filled by a caller's object literal.
export function loadOrder(id: string): string {
  return id;
}

export function runWith(deps: { get: (id: string) => string }): string {
  return deps.get("a");
}

export function placeOrder(): string {
  return runWith({ get: loadOrder });
}

// A factory-returned function handed to a parameter that runs it.
export function loadInvoice(id: string): string {
  return id;
}

export function makeLoader(): (id: string) => string {
  return loadInvoice;
}

export function runLoader(get: (id: string) => string): string {
  return get("b");
}

export function sendInvoice(): string {
  return runLoader(makeLoader());
}

// A call resolution by name already follows: one edge, as before.
export function plainCall(): string {
  return loadOrder("c");
}
