import { io } from "socket.io-client";
import type { Socket } from "socket.io-client";
import type { ClientToServerEvents, RunNotifyPayload, ServerToClientEvents } from "./events.js";

/// The socket is built in one method, parked on a class field, and the emits
/// that carry the contract happen in later methods on `this.notifications`.
export class SupervisorSession {
  private notifications?: Socket<ServerToClientEvents, ClientToServerEvents>;

  constructor(private readonly apiUrl: string) {}

  start() {
    this.notifications = this.createRunNotificationsSocket();
  }

  stop() {
    this.notifications?.disconnect();
  }

  subscribeToRuns(runIds: string[]) {
    this.notifications.emit("run:subscribe", { version: "1", runIds });
  }

  unsubscribeFromRuns(runIds: string[]) {
    this.notifications.emit("run:unsubscribe", { version: "1", runIds });
  }

  private createRunNotificationsSocket() {
    const wsUrl = new URL(this.apiUrl);
    wsUrl.pathname = "/worker";

    const socket = io(wsUrl.href, { transports: ["websocket"] });
    socket.on("run:notify", (payload: RunNotifyPayload) => {
      this.onRunNotification(payload);
    });
    socket.on("connect", () => {});

    return socket;
  }

  private onRunNotification(payload: RunNotifyPayload) {
    console.log("run notification", payload.runId);
  }
}
