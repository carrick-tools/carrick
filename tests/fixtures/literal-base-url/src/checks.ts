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
