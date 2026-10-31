import { Server } from "socket.io";

export type OrderUpdate = {
  orderId: string;
  status: string;
};

const io = new Server(4000);

io.on("connection", (socket) => {
  socket.on("order:update", (update: OrderUpdate) => {
    io.emit("order:shipped", { orderId: update.orderId, carrier: "post" });
  });
});
