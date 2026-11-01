import { Server } from "socket.io";
import type { UpdateRef } from "./contracts";
export type OrderUpdate = UpdateRef & {
  orderId: string;
  status: string;
};

const io = new Server(4000);

io.on("connection", (socket) => {
  socket.on("order:update", (update: OrderUpdate) => {
    io.emit("order:shipped", { orderId: update.orderId, carrier: "post" });
  });
});
