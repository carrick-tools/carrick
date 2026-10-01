import { io } from "@fixture/typed-socket";

const live = io("http://localhost:5000");

export function follow(symbol: string) {
  live.on("connect", () => console.log("connected"));
  live.on("price:updated", (payload) => console.log(payload));
  live.emit("price:subscribe", { symbol });
}
