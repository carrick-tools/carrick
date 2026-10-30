import { Hub, Peer } from "@fixture/rt";
import { Conn, Leaky, Orphan, Plain, Two } from "./conn";

const hub = new Hub(4001);

hub.on("connection", (peer) => {
  const conn = new Conn(peer);
  conn.register();
  conn.notify();
  new Plain(peer).done();
  new Two(peer).ping();
  new Leaky(peer).flush();
});

export function adopt(other: Peer): void {
  new Orphan(other).lost();
}
