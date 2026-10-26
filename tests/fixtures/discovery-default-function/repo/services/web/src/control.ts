type FetchLike = (url: string, init?: RequestInit) => Promise<Response>;

export async function propose(base: string, doFetch: FetchLike = fetch): Promise<unknown> {
  const res = await doFetch(`${base}/v1/controls`, { method: "POST" });
  return res.json();
}

export async function run(): Promise<unknown> {
  return propose("https://api.example.test");
}
