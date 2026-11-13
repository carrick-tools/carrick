import { useCached } from "cache-kit";
import { link } from "event-kit";
import { LoadClient } from "cache-kit";

export function readX() {
  return useCached({ key: ["x"], load: () => fetch("/api/x") });
}

export function readGiven(options) {
  return useCached(options);
}

export function readSpread(options) {
  return useCached({ ...options });
}

const conn = link("ws://x");

export function listen(): void {
  conn.on("open", () => {});
  conn.on("chat", () => {});
  conn.once("close", () => {});
}

const client = new LoadClient();

export function warm() {
  return client.prefetch({ key: ["p"], load: () => fetch("/api/p") });
}

export function warmGiven(options) {
  return client.prefetch(options);
}

export function send(vars) {
  return client.mutate(vars);
}
