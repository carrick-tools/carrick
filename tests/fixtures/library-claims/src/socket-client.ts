import { connect } from "@fixture/socket";

const socket = connect("http://localhost:4000");

export function start() {
  socket.on("connect", () => {});
  socket.on("order:accepted", (payload) => console.log(payload));
  socket.emit("order:place", { id: 1 });
}
