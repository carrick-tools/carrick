import { bus } from "./bus";
import { mirror } from "./mirror";

// An inline subscriber: no function name encloses it.
bus.subscribe("orders.created", async (message: { orderId: string }) => {
  await mirror(message.orderId);
});
