import type { Socket } from "@fixture/live-wire";
import { io } from "@fixture/live-wire";

type Authed = Socket & {
  authed?: boolean;
};

export const wireContext: { current: Authed | null } = { current: null };

function useJoin() {
  return (socket: Authed) => {
    socket.on("h:joined", () => {
      socket.authed = true;
    });
  };
}

export function connect() {
  const socket = io("http://localhost:3000") as Authed;
  useJoin()(socket);
  return socket;
}
