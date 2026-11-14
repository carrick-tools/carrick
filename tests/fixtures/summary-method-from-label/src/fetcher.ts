async function load(base: string): Promise<number> {
  const res = await fetch(`${base}/api/stock`, { headers: {} });
  return res.status;
}
export const loaded = () => load("/plain");
