declare function openPopup(): void;

export async function send(url: string, init: { method: string }) {
  const res = await fetch(url, init);
  return res.json();
}

// One function, three branches: the type the caller passes picks the request.
export function connect(params: { type: "OAUTH" | "TWO_STEP" | "JWT"; key: string }) {
  if (params.type === "TWO_STEP") {
    return send(`/auth/two-step/${params.key}`, { method: "POST" });
  }
  if (params.type === "JWT") {
    return send(`/auth/jwt/${params.key}`, { method: "POST" });
  }
  return openPopup();
}

// The control: one request, whatever the caller passes.
export function fetchOne(id: string) {
  return send(`/items/${id}`, { method: "GET" });
}
