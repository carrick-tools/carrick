import type { EventBody } from "./types";

type Sender = (event: EventBody) => Promise<void>;

const state = { sender: null as Sender | null };

/** The app wires the sender once at start; until then tracking is a no-op. */
export function configureTracking(options: { send: Sender }): void {
  state.sender = options.send;
}

function emit(event: EventBody): void {
  const sender = state.sender;
  if (!sender) return;
  void sender(event);
}

/**
 * Counts one click. The label only names the control in the console: it is
 * never part of what is sent, which is an event this function builds.
 */
export function trackClick(label: string, now = false): void {
  console.debug("click on", label);
  emit({ name: "click", at: Date.now() });
  if (now) console.debug("flush requested");
}
