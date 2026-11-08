import { Connection, Gateway, OnMessage, Peer, Traced } from "@fixture/gateway";

@Gateway()
export class LobbyGateway {
  @OnMessage("lobby.enter")
  enter(@Peer() peer: Connection, body: { seat: number }): string {
    return `${peer.id}:${body.seat}`;
  }

  @Traced("lobby.trace")
  @OnMessage("lobby.chat")
  chat(body: { text: string }): number {
    return body.text.length;
  }

  @Traced("lobby.idle")
  idle(): void {}
}
