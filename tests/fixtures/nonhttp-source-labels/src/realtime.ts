import { Server } from "socket.io";

const io = new Server();

io.on("connection", (socket) => {
  socket
    .on("chat:send", (message: string) => void message);
});
