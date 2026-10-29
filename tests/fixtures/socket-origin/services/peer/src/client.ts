import { Peer } from "@fixture/rt";

const peer = new Peer("wss://hub.internal");

peer.send("order.placed", { sku: "a-1" });
peer.listen("order.accepted", (ack: unknown) => {
  console.log(ack);
});

export function untraced(other: Peer): void {
  other.send("order.lost", { sku: "b-2" });
}
