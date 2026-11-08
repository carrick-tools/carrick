import { Server } from "@fixture/live-server";

const server = new Server();
server.emit("presence", { online: 1 });
server.emit("ticker", { price: 2 });
server.emit("notice", { text: "hi" });
