// Synthetic socket emits. Every name is invented; held-place-message.test.ts
// wraps the payloads below against the listener's parameter type.

type OrderUpdate = { id: string; status: string };

declare const socket: { emit(event: string, payload?: unknown, ack?: (reply: { ok: boolean }) => void): boolean };
declare const update: OrderUpdate;

export function pushUpdate(): void {
  socket.emit("order:update", { id: "o1", status: "paid" });
}

export function pushWithAck(): void {
  socket.emit("order:update", update, (reply) => {
    void reply.ok;
  });
}
