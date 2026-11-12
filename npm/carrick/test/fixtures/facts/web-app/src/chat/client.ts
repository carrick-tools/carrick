import { io } from "socket.io-client";
import type { Socket } from "socket.io-client";

export class ChatClient {
  private readonly socket: Socket = io("/chat");

  send = (text: string): void => {
    this.socket.emit("chat:send", { text });
  };
}
