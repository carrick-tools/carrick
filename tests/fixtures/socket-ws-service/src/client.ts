// The other end of the same plain-WebSocket connection, in a second service.
import WebSocket from "ws";

interface OrderAccepted {
  orderId: string;
  acceptedAt: string;
}

interface OrderRejected {
  orderId: string;
  reason: string;
}

const socket = new WebSocket(process.env.ORDERS_WS_URL ?? "ws://localhost:8080");

socket.on("open", () => {
  socket.send(
    JSON.stringify({ type: "order.created", sku: "sku_9", quantity: 2 }),
  );
});

// The third spelling of the receive side: a table keyed by the envelope's
// discriminator, dispatched on inside the delivery handler. Each handler's
// first parameter types the payload, so these rows carry an anchor the
// switch/comparison spellings cannot.
const handlers = {
  "order.accepted": (order: OrderAccepted) => {
    void order;
  },
  "order.rejected": (order: OrderRejected) => {
    void order;
  },
  // Not a handler, so not an event: the value has to be callable for the key
  // to name something the table dispatches to.
  retries: 3,
};

socket.on("message", (raw: string) => {
  const msg = JSON.parse(raw);
  handlers[msg.type]?.(msg.payload);
});

// A dynamic envelope name has no identity to key on and is skipped.
export function forward(eventName: string, payload: unknown): void {
  socket.send(JSON.stringify({ type: eventName, payload }));
}
