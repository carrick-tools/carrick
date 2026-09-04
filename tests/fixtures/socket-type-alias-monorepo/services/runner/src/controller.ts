import { io } from "socket.io-client";
import type { Socket } from "socket.io-client";

/// The file names its socket type once and then declares every field with the
/// alias, so the alias is the only thing that says what the field holds.
export type SupervisorSocket = Socket<ServerToClientEvents, ClientToServerEvents>;

type ServerToClientEvents = {
  "run:notify": (payload: { version: "1"; runId: string }) => void;
};

type ClientToServerEvents = {
  "run:start": (payload: { version: "1"; runId: string }) => void;
  "run:stop": (payload: { version: "1"; runId: string }) => void;
};

export class RunController {
  private socket: SupervisorSocket;

  constructor(private readonly apiUrl: string) {
    this.socket = io(this.apiUrl, { transports: ["websocket"] });
  }

  start(runId: string) {
    this.socket.emit("run:start", { version: "1", runId });
  }

  stop(runId: string) {
    this.socket.emit("run:stop", { version: "1", runId });
  }
}
