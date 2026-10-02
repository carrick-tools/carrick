import { Socket } from "@fixture/live";
import type { Typing } from "./events";
const socket = new Socket("wss://live.example/ws");

socket.on("chat", (message: unknown) => {
  console.log(message);
});

export function startTyping(event: Typing): void {
  socket.emit("typing", event);
}

export function joinRoom(room: string): void {
  socket.emit("join", { room });
}
