import { Gate, Hub, Peer } from "@fixture/rt";

const hub = new Hub(4000);

hub.on("connection", (peer) => {
  peer.listen("order.placed", (order: unknown) => {
    console.log(order);
  });
  peer.send("order.accepted", { ok: true });
  notify(peer);
});

hub.on("connection", onPeer);

function onPeer(peer: Peer): void {
  peer.send("order.rejected", { ok: false });
}

function notify(peer: Peer): void {
  peer.send("order.shipped", { ok: true });
}

const gate = new Gate();

gate.watch((info, peer) => {
  peer.send("order.gated", { info });
});
