export async function exchangeToken(path: string): Promise<string | null> {
  const res = await fetch(`/v1${path}`, {
    method: 'POST',
    body: JSON.stringify({ scopes: ['read'] }),
  });
  if (!res.ok) return null;
  const data = (await res.json()) as { token?: string };
  return data.token ?? null;
}
