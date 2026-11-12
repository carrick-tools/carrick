import { io } from "socket.io-client";

const socket = io("/chat");

export function sendMessage(text: string): void {
  socket.emit("chat:send", { text });
}
