import type { Server as HttpServer } from "node:http";
import { Server } from "socket.io";
import type { Socket } from "socket.io";

/// The per-connection socket is handed to a class that keeps it on a field,
/// so the listeners that serve the contract are on `this.socket`.
class WorkerConnection {
  private readonly rooms = new Set<string>();

  constructor(private readonly socket: Socket) {}

  register() {
    this.socket.on("run:subscribe", ({ runIds }) => {
      for (const runId of runIds) {
        this.rooms.add(runId);
      }
    });

    this.socket.on("run:unsubscribe", ({ runIds }) => {
      for (const runId of runIds) {
        this.rooms.delete(runId);
      }
    });

    this.socket.on("disconnect", () => {
      this.rooms.clear();
    });
  }

  notify(runId: string) {
    this.socket.emit("run:notify", { version: "1", runId });
  }
}

export function startWorkerGateway(httpServer: HttpServer) {
  const server = new Server(httpServer);

  server.on("connection", (connection) => {
    new WorkerConnection(connection).register();
  });

  return server;
}
