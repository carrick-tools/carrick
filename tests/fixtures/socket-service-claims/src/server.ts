import { Server } from "@fixture/rt-server";
import { createServer } from "http";

const httpServer = createServer();
const io = new Server(httpServer);

io.on("connection", (socket) => {
  socket.on("chat:message", (message: string) => {
    io.emit("chat:broadcast", { message, at: Date.now() });
  });

  socket.on("typing", () => {
    socket.broadcast.emit("user:typing", socket.id);
  });

  socket.on("disconnect", () => {
    io.emit("user:left", socket.id);
  });
});

httpServer.listen(4000);
