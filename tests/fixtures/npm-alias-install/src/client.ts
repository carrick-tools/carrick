import { Link } from "wirelink-v4";

const link = new Link("wss://hub.example");

export function join(room: string): void {
  link.send("hub.join", { room });
}

export function leave(room: string): void {
  link.send("hub.leave", { room });
}
