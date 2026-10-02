import { Socket } from "@fixture/live";

const socket = new Socket("wss://live.example/ws");

socket.on("chat", (message: unknown) => {
  console.log(message);
});

export function startTyping(room: string): void {
  socket.emit("typing", { room });
}

export function joinRoom(room: string): void {
  socket.emit("join", { room });
}
