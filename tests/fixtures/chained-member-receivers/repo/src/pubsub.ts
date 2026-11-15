import { Hub } from "fixture-hub";

// (p1) Held all the way.
export async function heldChannel() {
  const hub = new Hub();
  const region = hub.region("eu");
  const channel = region.channel("orders");
  await channel.publish(new Uint8Array());
}

// (p2) A member call on a member call's value, on a held client.
export async function chainedChannel(hub: Hub) {
  await hub.region("eu").channel("audit").publish(new Uint8Array());
}

// (p3) The whole chain in place.
export async function inPlaceChannel() {
  await new Hub().region("us").channel("events").publish(new Uint8Array());
}
