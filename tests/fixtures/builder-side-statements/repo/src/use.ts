import { guardedUrl, loggedUrl, plainUrl, segmentUrl } from "./builders";

export async function load(id: string, key: string) {
  await fetch(guardedUrl(id, key), { method: "GET" });
  await fetch(plainUrl(key), { method: "GET" });
  await fetch(loggedUrl(key), { method: "GET" });
  await fetch(segmentUrl(key), { method: "GET" });
}
