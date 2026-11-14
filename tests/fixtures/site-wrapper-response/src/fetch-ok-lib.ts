export async function ping(base: string) {
  const res = await fetch(`${base}/health`, { method: "POST" });
  return res.ok;
}
