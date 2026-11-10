// Each wrapper resolves a path against a base it reads at run time. The
// request names its route at the caller, so a caller's path is the route.
declare function apiBase(): string;

// The URL is built in the wrapper and handed to `fetch` as text.
export async function removeItem(path: string) {
  const res = await fetch(new URL(path, apiBase()).toString(), { method: "POST" });
  return res.json();
}

// The URL object itself is handed to `fetch`.
export async function removeItemByUrl(path: string) {
  const res = await fetch(new URL(path, apiBase()), { method: "POST" });
  return res.json();
}

// A builder returns the URL text built from its own parameters.
function urlFor(base: string, path: string) {
  return new URL(path, base).toString();
}

export async function removeItemThroughBuilder(path: string) {
  const res = await fetch(urlFor(apiBase(), path), { method: "POST" });
  return res.json();
}
