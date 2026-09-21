// Both bases are declared once as module-level string literals and
// interpolated at the call site. The host is stated outright in this file, so
// each request is the absolute-host form written a binding away.
const ADMIN_API = "http://localhost:8080";
const APP_URL = "http://localhost:3030";

export async function healthCheck() {
  const statuses = await fetch(`${ADMIN_API}/status`);

  const app = await fetch(`${APP_URL}/api/v1/whoami`, {
    headers: { Authorization: `Bearer ${process.env.SERVICE_SECRET_KEY}` },
  });

  return { statuses: statuses.ok, app: app.ok };
}

// The same base, at a site the cassette holds no row for, with the verb written
// in the call's own options bag. Neither of the two sites above states a verb
// anywhere — a bare `fetch(url)` and a bag carrying only headers — so this is
// the one site the scanner can state a whole row for on its own (carrick#641).
export async function evictCache(id: string) {
  return fetch(`${ADMIN_API}/cache/${id}`, {
    method: "DELETE",
  });
}
