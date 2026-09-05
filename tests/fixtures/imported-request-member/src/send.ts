// A transport helper: the schema comes first, so the URL is NOT the first
// argument. Nothing in the scanner recognises a call to this as a request.
export async function send<T>(
  schema: T,
  url: string,
  init: RequestInit,
  options?: { retries?: number }
): Promise<unknown> {
  const response = await fetch(url, init);
  return response.json();
}

export function mergeOptions(a: unknown, b: unknown) {
  return { ...(a as object), ...(b as object) };
}

// The same transport, paginated. The page bag sits BETWEEN the URL and the
// request-options bag, so a call to it carries two object literals and the
// options bag is no longer the only one.
export async function sendPage<T>(
  schema: T,
  url: string,
  page: { page?: number; limit?: number },
  init: RequestInit,
  options?: { retries?: number }
): Promise<unknown> {
  const response = await fetch(url, init);
  return response.json();
}
