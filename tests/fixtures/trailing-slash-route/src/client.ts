export async function listEnvironments(): Promise<string[]> {
  const res = await fetch('/api/v1/environments');
  return (await res.json()).environments;
}
