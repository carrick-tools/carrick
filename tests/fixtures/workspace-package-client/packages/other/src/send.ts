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
