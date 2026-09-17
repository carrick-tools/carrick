// The other end of the same plain-WebSocket connection, in a second service.
import WebSocket from "ws";

const socket = new WebSocket(process.env.ORDERS_WS_URL ?? "ws://localhost:8080");

socket.on("open", () => {
  socket.send(
    JSON.stringify({ type: "order.created", sku: "sku_9", quantity: 2 }),
  );
});

// A dynamic envelope name has no identity to key on and is skipped.
export function forward(eventName: string, payload: unknown): void {
  socket.send(JSON.stringify({ type: eventName, payload }));
}
