export async function streamPost(path: string, body: unknown): Promise<Response> {
  const res = await fetch(`/v1${path}`, { method: 'POST', body: JSON.stringify(body ?? {}) });
  return res;
}
