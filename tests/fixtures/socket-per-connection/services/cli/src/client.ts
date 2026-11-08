import { Socket } from "socket.io-client";

export function start(socket: Socket): void {
  socket.emit("chat.in", { text: "hi" });
  socket.on("chat.hello", (m: unknown) => console.log(m));
  socket.on("chat.reply", (m: unknown) => console.log(m));
  socket.on("chat.fan", (m: unknown) => console.log(m));
  socket.on("chat.notice", (m: unknown) => console.log(m));
}
