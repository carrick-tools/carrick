import type { Server as HttpServer } from "node:http";
import type { Namespace } from "socket.io";
import { Server } from "socket.io";

/// The server is built in one function and the namespace is carved off it in
/// another, so only the namespace's declared type says what it is.
function createSocketServer(httpServer: HttpServer) {
  const io = new Server(httpServer);

  io.on("connection", (socket) => {
    socket.on("session:hello", ({ id }) => {
      console.log("default namespace", id);
    });
  });

  return io;
}

function createWorkerNamespace({ io, namespace }: { io: Server; namespace: string }) {
  const worker: Namespace = io.of(namespace);

  worker.on("connection", async (socket) => {
    socket.on("run:subscribe", async ({ runIds }) => {
      for (const runId of runIds) {
        socket.join(runId);
      }
    });

    socket.on("run:unsubscribe", async ({ runIds }) => {
      for (const runId of runIds) {
        socket.leave(runId);
      }
    });

    socket.on("disconnect", () => {});
  });

  return worker;
}

export function startPlatform(httpServer: HttpServer) {
  const io = createSocketServer(httpServer);

  return {
    io,
    workerNamespace: createWorkerNamespace({ io, namespace: "/worker" }),
  };
}
