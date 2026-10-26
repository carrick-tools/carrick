type FetchLike = (url: string, init?: RequestInit) => Promise<Response>;

export async function load(base: string, doFetch = (url: string, init?: RequestInit) => fetch(url, init)) {
  const res = await doFetch(`${base}/v1/items`);
  return res.json();
}

export async function plain(): Promise<unknown> {
  return load("https://api.example.test");
}

export async function own(): Promise<unknown> {
  const stub: FetchLike = async () => new Response("{}");
  return load("https://api.example.test", stub);
}
