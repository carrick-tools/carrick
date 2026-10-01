const endpoint = new URL("/api/member-module", process.env.SERVICE_URL);

export function memberShadow(flag: boolean) {
  if (flag) {
    const endpoint = "/api/member-local";
    return fetch(endpoint, { method: "PUT" });
  }
  return null;
}

export function memberPlain() {
  return fetch(endpoint, { method: "GET" });
}
