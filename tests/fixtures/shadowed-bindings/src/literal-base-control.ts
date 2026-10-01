const STATUS_BASE = "http://status.example.com";

export async function checkStatus() {
  return fetch(`${STATUS_BASE}/health`, { method: "GET" });
}
