import type { Socket } from "socket.io";

export class ChatGateway {
  private readonly rooms = new Map<string, Socket[]>();

  onSend = (socket: Socket, payload: { text: string }): void => {
    socket.broadcast.emit("chat:message", payload);
  };
}
