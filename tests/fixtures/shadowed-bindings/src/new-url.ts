const target = new URL("/api/new-url-module", process.env.SERVICE_URL);

export async function newUrlShadow(raw: boolean) {
  if (raw) {
    const target = "/api/new-url-raw";
    return fetch(target, { method: "POST" });
  }
  return null;
}

export async function newUrlModule() {
  return fetch(target, { method: "GET" });
}
