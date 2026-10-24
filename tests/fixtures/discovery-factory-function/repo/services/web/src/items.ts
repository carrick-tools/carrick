import { makeClient } from "./http";

export async function search(): Promise<unknown> {
  const send = makeClient({ token: "t" });
  return send("POST", "/v1/items/search", {});
}

export async function one(): Promise<unknown> {
  const send = makeClient({ token: "t" });
  return send("GET", "/v1/items/1");
}
