export type ApiRequestOptions = { method: "GET" | "POST"; url: string };
export async function request<T>(options: ApiRequestOptions): Promise<T> {
  const res = await fetch(`${process.env.API_URL}${options.url}`, { method: options.method });
  return (await res.json()) as T;
}
