import { Server } from "@fixture/live-server";

const server = new Server();
server.emit("presence", { online: 1 });
server.emit("ticker", { price: 2 });
server.emit("notice", { text: "hi" });
server.emit("w:alias", {});
server.emit("w:tagged", {});
server.emit("w:type-import", {});
server.emit("w:lifecycle", {});
server.emit("h:joined", {});
server.emit("s:message", {});
server.emit("s:bus", {});
server.emit("data", {});
const K = "emit" as const;
server[K]("k:const", {});
