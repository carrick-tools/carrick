import { bus } from "./bus";

export function placeOrder(order: { id: string }): void {
  bus.emit("orderPlaced", order);
}

bus.on("orderPlaced", (order: { id: string }) => {
  console.log("placed", order.id);
});
