import { queue, rpc } from "./transports.js";
import { makeStore } from "./store.js";
import type { Deps, Procedures } from "./ports.js";

export const procedures: Procedures = {
  getUser: (id) => rpc.call("getUser"),
};

function publishTopic(t: string) {
  queue.publish(t);
}

export const deps: Deps = { publish: publishTopic, store: makeStore() };
