// A plain-WebSocket server. The library gives no client/server signal — the
// same `WebSocket` type is used on both ends — so every op extracted here
// carries an unknown direction.
//
// The contract names are NOT in the call: `ws` has one `message` event, and the
// application's event name is the discriminator of the serialized envelope, on
// the way out and on the way back in.
import { WebSocketServer, WebSocket } from "ws";
import { EventEmitter } from "node:events";

interface OrderAccepted {
  orderId: string;
  acceptedAt: string;
}

const server = new WebSocketServer({ port: 8080 });

// A name held in a binding, not written at the case: nothing to key on.
const REPLAY_EVENT = "order.replayed";

// Not a contract event: `connection` is the transport's own lifecycle.
server.on("connection", (socket: WebSocket) => {
  // Neither is `message` — the envelope inside it carries the event names, and
  // the handler names them as the literals it compares the discriminator to.
  socket.on("message", (raw: string) => {
    const msg = JSON.parse(raw);
    switch (msg.type) {
      case "order.created":
        accept(msg.orderId);
        break;
      case "order.cancelled":
        release(msg.orderId);
        break;
      // Not a literal at the case, so it is skipped exactly as a dynamic
      // envelope name is on the sending side.
      case REPLAY_EVENT:
        break;
      default:
        // A second discriminator read in the same handler, spelled as a
        // comparison instead of a case.
        if (msg.type === "order.amended") {
          amend(msg.orderId);
        }
        // `status` is not the envelope's discriminator key, so the literals
        // compared against it are not event names.
        if (msg.status === "pending") {
          return;
        }
    }
  });

  const accepted: OrderAccepted = {
    orderId: "ord_1",
    acceptedAt: new Date().toISOString(),
  };

  socket.send(JSON.stringify({ type: "order.accepted", ...accepted }));

  socket.send(JSON.stringify({ type: "order.rejected", orderId: "ord_2" }));
});

// The same discriminator read OUTSIDE a delivery handler: there is no transport
// registration to attribute it to, so it is not a listener.
export function summarize(raw: string): string {
  const msg = JSON.parse(raw);
  switch (msg.type) {
    case "order.archived":
      return "archived";
    default:
      return "other";
  }
}

function accept(orderId: string): void {
  void orderId;
}

function release(orderId: string): void {
  void orderId;
}

function amend(orderId: string): void {
  void orderId;
}

// An in-process emitter in the same file: not imported from a socket client,
// so it is not a socket root and produces no socket row.
const audit = new EventEmitter();
audit.emit("order.audited", { orderId: "ord_1" });
