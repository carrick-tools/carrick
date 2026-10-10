import { Subscriber } from "@fixture/broker";

const subscriber = new Subscriber(process.env.BROKER_URL);

subscriber.on("invoicePaid", (invoice: { id: string }) => {
  console.log("paid", invoice.id);
});
