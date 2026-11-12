import { Server } from "socket.io";

export const io = new Server();
io.on("connection", (socket) => {
  socket.on("chat:send", (payload: { text: string }) => {
    io.emit("chat:message", payload);
  });
});
