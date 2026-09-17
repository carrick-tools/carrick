// A plain-WebSocket server. The library gives no client/server signal — the
// same `WebSocket` type is used on both ends — so every op extracted here
// carries an unknown direction.
//
// The contract names are NOT in the call: `ws` has one `message` event, and the
// application's event name is the discriminator of the serialized envelope.
import { WebSocketServer, WebSocket } from "ws";
import { EventEmitter } from "node:events";

interface OrderAccepted {
  orderId: string;
  acceptedAt: string;
}

const server = new WebSocketServer({ port: 8080 });

// Not a contract event: `connection` is the transport's own lifecycle.
server.on("connection", (socket: WebSocket) => {
  // Neither is `message` — the envelope inside it carries the event name.
  socket.on("message", (raw: string) => {
    const accepted: OrderAccepted = JSON.parse(raw);
    void accepted;
  });

  socket.send(
    JSON.stringify({
      type: "order.accepted",
      orderId: "ord_1",
      acceptedAt: new Date().toISOString(),
    }),
  );

  socket.send(JSON.stringify({ type: "order.rejected", orderId: "ord_2" }));
});

// An in-process emitter in the same file: not imported from a socket client,
// so it is not a socket root and produces no socket row.
const audit = new EventEmitter();
audit.emit("order.audited", { orderId: "ord_1" });
