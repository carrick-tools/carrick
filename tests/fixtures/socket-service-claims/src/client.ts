import { io } from "@fixture/rt-client";

const socket = io("https://chat.internal");

socket.on("connect", () => {
  socket.emit("chat:message", "hello");
});

socket.on("chat:broadcast", (payload: { message: string; at: number }) => {
  console.log(payload.message);
});

// Emitted by the client but never handled by the server fixture.
socket.emit("presence:ping", Date.now());

// Dynamic event names never become contract events.
socket.emit(`metrics:${process.env.SHARD}`, {});
