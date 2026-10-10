import { EventEmitter } from "node:events";

// The service's own in-process bus.
export const bus = new EventEmitter();
