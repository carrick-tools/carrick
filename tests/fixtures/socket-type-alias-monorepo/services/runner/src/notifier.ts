import type { SupervisorSocket } from "./controller.js";

/// This file never names the socket type: it imports the alias its sibling
/// declares, and the field is the only thing that says what it holds.
export class RunNotifier {
  private socket: SupervisorSocket;

  constructor(opts: { supervisorSocket: SupervisorSocket }) {
    this.socket = opts.supervisorSocket;
  }

  start() {
    this.socket.on("run:notify", ({ runId }) => {
      console.log("notified", runId);
    });
  }
}
