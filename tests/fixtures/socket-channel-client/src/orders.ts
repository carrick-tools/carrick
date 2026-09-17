// A channel-style realtime client: one object opens a channel, and the channel
// carries named events. The package name here is a neutral stand-in — the pass
// reads it from the detector's `socket_clients`, never from a list in Rust, so
// any package with this shape resolves the same way.
import { ChannelClient, type Channel } from "realtime-channels";

interface OrderCreated {
  orderId: string;
  sku: string;
}

const client = new ChannelClient({ key: process.env.REALTIME_KEY ?? "" });

// The channel comes back from a method call, which is the only way to reach it.
const orders = client.subscribe("orders");

orders.bind("order.created", (payload: OrderCreated) => {
  void payload;
});

orders.trigger("order.viewed", { orderId: "ord_1" });

// A second channel, reached through its declared type instead.
const shipments: Channel = client.channel("shipments");

shipments.bind("shipment.dispatched", handleDispatched);

function handleDispatched(payload: unknown): void {
  void payload;
}

// No handler argument: this opens a channel, it does not register anything, so
// it is not a listener on an event called "presence".
export const presence = client.subscribe("presence");
