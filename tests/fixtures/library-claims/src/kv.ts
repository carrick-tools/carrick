import { KvClient } from "@fixture/kv";

const kv = new KvClient("kv://localhost:6379");
const ORDERS = "orders.created";

export async function publishOrder(id: string) {
  await kv.publish(ORDERS, id);
}

export async function listen() {
  await kv.subscribe("orders.created", (message) => console.log(message));
  await kv.psubscribe("orders.*", (message) => console.log(message));
}

export async function relay(ORDERS: string) {
  await kv.publish(ORDERS, "relayed");
}
