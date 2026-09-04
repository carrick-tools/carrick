import type { Server as HttpServer } from "node:http";
import type { Socket } from "socket.io";
import { Server } from "socket.io";

/// The server side names its per-connection socket type once too.
type WorkloadSocket = Socket<ClientToServerEvents, ServerToClientEvents>;

type ClientToServerEvents = {
  "run:start": (payload: { version: "1"; runId: string }) => void;
  "run:stop": (payload: { version: "1"; runId: string }) => void;
};

type ServerToClientEvents = {
  "run:notify": (payload: { version: "1"; runId: string }) => void;
};

class WorkloadConnection {
  constructor(private readonly socket: WorkloadSocket) {}

  register() {
    this.socket.on("run:start", ({ runId }) => {
      console.log("start", runId);
    });

    this.socket.on("run:stop", ({ runId }) => {
      console.log("stop", runId);
    });
  }
}

export function startWorkloadServer(httpServer: HttpServer) {
  const server = new Server(httpServer);

  server.on("connection", (connection) => {
    new WorkloadConnection(connection).register();
  });

  return server;
}
