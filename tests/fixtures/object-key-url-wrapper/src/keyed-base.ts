// The base arrives as a key, and the path is written here. The request states
// its route without the caller, so it is stated at its own line, with the base
// under the name the function reads it by: exactly as it was before a key
// could be filled in.
export async function listWidgets({ apiUrl }: { apiUrl: string }) {
  const url = `${apiUrl}/widgets`;
  const res = await fetch(url, { method: "GET" });
  return res.json();
}

export async function showWidgets() {
  return await listWidgets({ apiUrl: process.env.WIDGETS_API_URL ?? "" });
}

// The same through a member read, with a path parameter.
export async function loadWidget(options: { apiUrl: string; id: string }) {
  const url = `${options.apiUrl}/widgets/${options.id}`;
  const res = await fetch(url, { method: "GET" });
  return res.json();
}
