import { loadContext } from "@/session/lookup";

export function resume(next) {
  return loadContext(next);
}

export function ping() {
  return fetch("/health");
}
