import { io } from "socket.io-client";
import type { Socket } from "socket.io-client";

/// The publisher side of the same events: a client socket parked on a class
/// field, connecting to the namespace through the URL path.
export class WorkerSession {
  private notifications?: Socket;

  constructor(private readonly apiUrl: string) {}

  start() {
    const url = new URL(this.apiUrl);
    url.pathname = "/worker";
    this.notifications = io(url.href, { transports: ["websocket"] });
  }

  subscribeToRuns(runIds: string[]) {
    this.notifications.emit("run:subscribe", { version: "1", runIds });
  }

  unsubscribeFromRuns(runIds: string[]) {
    this.notifications.emit("run:unsubscribe", { version: "1", runIds });
  }
}
