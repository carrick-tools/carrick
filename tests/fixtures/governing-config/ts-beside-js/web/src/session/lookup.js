import { config } from "@/config";

export function loadContext(returnTo) {
  const id = new URL(returnTo, location.origin).searchParams.get("ticket");
  return fetch(`${config.apiUrl.replace(/\/$/, "")}/sessions/${id}`);
}
