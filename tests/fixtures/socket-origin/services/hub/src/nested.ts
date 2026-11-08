import { Hub, Peer } from "@fixture/rt";

const hub = new Hub(4100);

// Own code that keeps the function it is handed and never runs it.
class LocalBus {
  private handlers: Array<() => void> = [];
  add(handler: () => void): void {
    this.handlers.push(handler);
  }
}
const local = new LocalBus();
class OwnRunner {
  private kept: Array<(peer: Peer) => void> = [];
  add(handler: (peer: Peer) => void): void {
    this.kept.push(handler);
  }
}
const local2 = new OwnRunner();

// A `peer` the `connection` callback owns, read in callbacks nested in it.
hub.on("connection", (peer) => {
  peer.send("k.direct", {});
  peer.listen("k.asked", () => {
    peer.send("k.answered", {});
  });
  local.add(() => {
    peer.send("k.local", {});
  });
  setTimeout(() => {
    peer.send("k.later", {});
  }, 10);
  peer.listen("k.deep", () => {
    [1, 2].forEach(() => {
      peer.send("k.deeper", {});
    });
  });
  // A named nested function has no host to step out through.
  function reply(): void {
    peer.send("k.named", {});
  }
  reply();
});

hub.on("connection", function (peer) {
  peer.listen("k.fn-asked", function () {
    peer.send("k.fn-answered", {});
  });
});

// The owner is handed to own code that keeps it: nothing hands it a peer.
local2.add((peer: Peer) => {
  setTimeout(() => {
    peer.send("k.own-host", {});
  }, 0);
});

// The inner callback's own `peer` shadows the outer one.
hub.on("connection", (peer) => {
  peer.listen("k.shadow-in", (peer: Peer) => {
    peer.send("k.shadow", {});
  });
});

// The owner sits inside a `connection` callback but is itself handed to own
// code: the walk stops at the owner, it does not hop on to `hub.on`.
hub.on("connection", (peer) => {
  local2.add((p: Peer) => {
    setTimeout(() => {
      p.send("k.inner-own", {});
    }, 0);
  });
});
