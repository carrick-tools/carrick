import { Peer } from "@fixture/rt";

declare function track(owner: object): void;

// A parameter property: the field is the constructor's parameter.
export class Conn {
  constructor(private readonly peer: Peer) {}

  register(): void {
    this.peer.listen("run.start", (job: unknown) => {
      console.log(job);
    });
  }

  notify(): void {
    this.peer.send("run.notify", { ok: true });
  }
}

// One `this.field = parameter` in the constructor.
export class Plain {
  private peer: Peer;

  constructor(peer: Peer) {
    this.peer = peer;
  }

  done(): void {
    this.peer.send("run.done", { ok: true });
  }
}

// A second write: the field may hold another peer.
export class Two {
  constructor(private peer: Peer) {}

  reset(next: Peer): void {
    this.peer = next;
  }

  ping(): void {
    this.peer.send("run.ping", { ok: true });
  }
}

// `this` leaves the class: something else may set the field.
export class Leaky {
  constructor(private peer: Peer) {
    track(this);
  }

  flush(): void {
    this.peer.send("run.flush", { ok: true });
  }
}

// Built with a peer of the client's end, in another service's shape: the
// parameter is handed a peer nothing in the service hands in.
export class Orphan {
  constructor(private peer: Peer) {}

  lost(): void {
    this.peer.send("run.lost", { ok: true });
  }
}
