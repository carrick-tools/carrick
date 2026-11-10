import { bus } from "./bus";

export function placeOrder(id: string): void {
  bus.emit("orderPlaced", id);
}

bus.on("orderPlaced", (id: string) => {
  console.log("placed", id);
});
