export async function readUp(base: string) {
  const res = await fetch(`${base}/up`, { method: "POST" });
  return (await res.json()) as { up: boolean };
}
