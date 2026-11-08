import { Namespace, Server, Socket } from "socket.io";

const io = new Server(3000);

io.on("connection", (socket) => {
  socket.on("chat.in", (message: unknown) => {
    console.log(message);
  });
  socket.emit("chat.hello", { ok: true });
});

export function reply(client: Socket): void {
  client.emit("chat.reply", { ok: true });
}

export function fanOut(rooms: Map<string, Set<Socket>>, key: string): void {
  for (const member of rooms.get(key) ?? []) {
    member.emit("chat.fan", { key });
  }
}

export function announce(space: Namespace): void {
  space.emit("chat.notice", { ok: true });
}
