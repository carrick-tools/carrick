import { EventEmitter } from "node:events";

export const refunds = new EventEmitter();

refunds.on("invoice.refunded", (event: unknown) => {
  void event;
});
