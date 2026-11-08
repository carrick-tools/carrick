import { Link } from "@fixture/gateway";

const link = new Link("wss://lobby.example");

export function enter(seat: number): void {
  link.send("lobby.enter", { seat });
}

export function chat(text: string): void {
  link.send("lobby.chat", { text });
}
