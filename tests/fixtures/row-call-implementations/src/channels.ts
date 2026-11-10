import { socket } from "./transports.js";
import { Channel } from "./ports.js";

export class SocketChannel extends Channel {
  open() {
    socket.emit("open");
  }
}
