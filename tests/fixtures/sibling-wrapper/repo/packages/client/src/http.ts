export async function getItem(id: string) {
  const res = await fetch(`/api/items/${id}`);
  return res.json();
}

export async function getHealth() {
  const res = await fetch("/api/health");
  return res.json();
}

export async function getStatus() {
  const res = await fetch("/api/status");
  return res.json();
}

export async function getVersion() {
  const res = await fetch("/api/version");
  return res.json();
}
