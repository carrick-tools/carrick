import { send } from "@fixture/client";

function post(url: string) {
  return send(url, { method: "POST" });
}

// The same shape inside this service's own file.
function sync(kind: "PUSH" | "PULL", key: string) {
  if (kind === "PUSH") {
    return post(`/sync/push/${key}`);
  }
  return post(`/sync/pull/${key}`);
}

export function syncOnce() {
  return sync("PUSH", "k1");
}
