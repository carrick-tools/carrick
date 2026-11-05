import { Socket } from "fixture-socket";

export interface Channel {
  push(name: string): unknown;
}

class HttpChannel implements Channel {
  push(name: string): unknown {
    return fetch(`/channels/${name}`, { method: "PUT" });
  }
}

class LiveChannel implements Channel {
  constructor(private readonly socket: Socket) {}

  push(name: string): unknown {
    return this.socket.send(name, {});
  }
}

const channels: Record<string, Channel> = {
  http: new HttpChannel(),
  live: new LiveChannel(new Socket("wss://channels.example")),
};

export function pushTo(kind: string): unknown {
  return channels[kind].push("hello");
}
