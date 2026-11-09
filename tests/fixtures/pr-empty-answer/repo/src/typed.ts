import type { Server } from "role-server";
import type { HttpClient } from "role-fetcher";

// The same two shapes again, with each receiver handed in as a parameter: the
// file imports only the types, so what the receiver IS comes from its declaration.
export function mount(server: Server): void {
  server.get("/gadgets", (req, res) => res.send("ok"));
}

export function load(client: HttpClient): Promise<unknown> {
  return client.get("/gadgets", { retries: 2 });
}
