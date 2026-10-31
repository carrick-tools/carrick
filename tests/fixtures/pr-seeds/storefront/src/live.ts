import { io } from "socket.io-client";

export type OrderUpdate = {
  orderId: string;
  status: string;
};

const socket = io("https://orders.internal");

export function sendUpdate(orderId: string): void {
  const update: OrderUpdate = { orderId, status: "paid" };
  socket.emit("order:update", update);
}

socket.on("order:shipped", (event: { orderId: string; carrier: string }) => {
  void event.carrier;
});
