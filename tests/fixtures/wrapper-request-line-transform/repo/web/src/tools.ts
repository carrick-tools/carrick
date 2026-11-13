type Outcome = { ok: true; data: unknown } | { ok: false; status: number };

async function getJson(origin: string, path: string, token: string): Promise<Outcome> {
  const res = await fetch(`${origin}${path}`, {
    headers: { Authorization: `Bearer ${token}`, Accept: 'application/json' },
  });
  if (!res.ok) return { ok: false, status: res.status };
  return { ok: true, data: await res.json() };
}

async function exchangeToken(origin: string, account: string): Promise<string | null> {
  const res = await fetch(`${origin}/v1/accounts/${account}/tokens`, {
    method: 'POST',
    body: JSON.stringify({ scopes: ['read'] }),
  });
  if (!res.ok) return null;
  const data = (await res.json()) as { token?: string };
  return data.token ?? null;
}

export function buildTools(origin: string, account: string) {
  let tokenPromise: Promise<string | null> | undefined;
  function getToken(): Promise<string | null> {
    tokenPromise ??= exchangeToken(origin, account);
    return tokenPromise;
  }

  return {
    listTickets: async () => {
      const token = await getToken();
      if (!token) return { error: 'no token' };
      const result = await getJson(origin, '/v1/tickets', token);
      if (!result.ok) return { error: `status ${result.status}` };
      return result.data;
    },
  };
}
