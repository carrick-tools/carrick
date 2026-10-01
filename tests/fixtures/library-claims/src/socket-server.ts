import { Hub } from "@fixture/socket";

const hub = new Hub({ port: 4000 });

export function serve() {
  hub.on("order:place", (payload) => console.log(payload));
  hub.emit("order:accepted", { ok: true });
}
