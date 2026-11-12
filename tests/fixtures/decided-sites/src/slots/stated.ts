import { useCached } from "cache-kit";
import { buildOptions } from "./shared";
import { link } from "event-kit";
import { LoadClient } from "cache-kit";

export function readX() {
  return useCached({ key: ["x"], load: () => fetch("/api/x") });
}

const options = { key: ["g"], load: () => fetch("/api/g") };
const bare = { key: ["z"] };

function loadOptions(id) {
  return { key: [id], load: () => fetch(`/api/h/${id}`) };
}

export function readGiven() {
  return useCached(options);
}

export function readFactory(id) {
  return useCached(loadOptions(id));
}

export function readBuilt() {
  return useCached(buildOptions("x"));
}

export function readBare() {
  return useCached(bare);
}

export function readParam(given) {
  return useCached(given);
}

export function readSpread(extra) {
  return useCached({ ...extra });
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
