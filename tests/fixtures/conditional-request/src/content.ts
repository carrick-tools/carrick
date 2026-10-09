declare function lookup(kind: string): string;

export async function publish(kind: string, id: string) {
  const segment = kind === "draft" ? `drafts/${id}` : `posts/${id}`;
  await fetch(`/api/content/${segment}/publish`, { method: "POST" });
}

export async function publishBound(kind: string, id: string) {
  const segment = kind === "draft" ? `drafts/${id}` : `posts/${id}`;
  const url = `/api/content/${segment}/publish`;
  await fetch(url, { method: "POST" });
}

export async function publishInline(kind: string, id: string) {
  await fetch(kind === "draft" ? `/api/content/drafts/${id}/publish` : `/api/content/posts/${id}/publish`, { method: "POST" });
}

export async function publishUnreadable(kind: string, id: string) {
  const segment = kind === "draft" ? `drafts/${id}` : lookup(kind);
  const url = `/api/content/${segment}/publish`;
  await fetch(url, { method: "POST" });
}

export async function publishUnreadableInline(kind: string, id: string) {
  const segment = kind === "draft" ? `drafts/${id}` : lookup(kind);
  await fetch(`/api/content/${segment}/publish`, { method: "POST" });
}

export async function health(local: boolean) {
  const base = local ? "http://localhost:3000" : process.env.CONTENT_API_URL;
  const url = `${base}/health`;
  await fetch(url, { method: "GET" });
}

export async function openValue(shared: boolean, ownerId: string, groupId: string) {
  const owner = shared ? groupId : ownerId;
  const url = `/api/content/${owner}/items`;
  await fetch(url, { method: "GET" });
}

export async function list(archived: boolean) {
  const filter = archived ? "?archived=1" : "";
  const url = `/api/content${filter}`;
  await fetch(url, { method: "GET" });
}
