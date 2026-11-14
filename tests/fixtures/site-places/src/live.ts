import { Socket } from "socket.io-client";

export function start(socket: Socket): void {
  socket.emit("chat.in", { text: "hi" });
}
