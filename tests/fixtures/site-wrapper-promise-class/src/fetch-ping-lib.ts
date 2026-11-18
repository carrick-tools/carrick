export async function ping(b: string) {
  const res = await fetch(`${b}/health`);
  return res.ok;
}
