import { Link } from "@fixture/gateway";

const link = new Link("wss://lobby.example");

export function enter(seat: number): void {
  link.send("lobby.enter", { seat });
}

export function chat(text: string): void {
  link.send("lobby.chat", { text });
}

export function wrapped(): void {
  link.send("w:wrapped", {});
}

export function combined(): void {
  link.send("w:combined", {});
}

export function named(): void {
  link.send("w:const", {});
}

export function imported(): void {
  link.send("w:imported", {});
}
