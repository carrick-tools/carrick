import { io, Socket } from "@fixture/live-wire";

const local: Socket = io("http://localhost:3000");
local.on("presence", () => undefined);

export class Feed {
  constructor(private readonly socket: Socket) {}

  start(): void {
    this.socket.on("ticker", () => undefined);
  }
}

export class Holder {
  private inner: Socket = io("http://localhost:3000");

  get connection(): Socket {
    return this.inner;
  }

  start(): void {
    this.connection.once("notice", () => undefined);
  }
}
