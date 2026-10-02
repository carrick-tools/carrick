import { bus } from "@fixture/queue";

bus.subscribe("orders.created", async (payload: unknown) => {
  console.log("send the confirmation", payload);
});
