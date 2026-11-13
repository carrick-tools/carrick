type Outcome = { ok: true; data: unknown } | { ok: false; status: number };

export async function getJson(path: string): Promise<Outcome> {
  const res = await fetch(`/v1${path}`);
  if (!res.ok) return { ok: false, status: res.status };
  return { ok: true, data: await res.json() };
}
