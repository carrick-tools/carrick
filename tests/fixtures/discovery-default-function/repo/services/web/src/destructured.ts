type Options = { base: string; send?: (url: string, init?: RequestInit) => Promise<Response> };

export async function notify({
  base,
  send = (url, init) => fetch(url, init),
}: Options): Promise<unknown> {
  const res = await send(`${base}/v1/notices`, { method: "POST" });
  return res.json();
}

export async function run(): Promise<unknown> {
  return notify({ base: "https://api.example.test" });
}
