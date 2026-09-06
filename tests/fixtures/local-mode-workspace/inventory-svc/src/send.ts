// The request helper the client's members call. The path and the verb are
// stated at the call site, which is what makes the row deterministic.

export type RequestOptions = {
  method: string;
  headers?: Record<string, string>;
};

export async function send<T>(url: string, options: RequestOptions): Promise<T> {
  const response = await fetch(url, options);
  return (await response.json()) as T;
}
