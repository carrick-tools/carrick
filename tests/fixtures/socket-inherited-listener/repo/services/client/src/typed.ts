import { io, Socket } from "@fixture/live-wire";
import type { Socket as T } from "@fixture/live-wire";

type Wire = Socket<{ "w:in": () => void }, {}>;
type Tagged = Socket & { tagged?: boolean };

const handler = () => undefined;

export class Aliased {
  private inner: { wire: Wire } = { wire: io("http://localhost:3000") as Wire };

  get wire(): Wire {
    return this.inner.wire;
  }

  start(): void {
    this.wire.on("w:alias", handler);
  }
}

export const handlers = () => (socket: Tagged) => {
  socket.on("w:tagged", handler);
};

export const typeOnly = (peer: T) => peer.on("w:type-import", handler);

export const lifecycle = (socket: Socket) => {
  socket.io.on("w:lifecycle", handler);
};
