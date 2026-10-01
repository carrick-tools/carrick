import { KvClient } from "@fixture/kv";

// One client for a cache read and a publish: the read is a call no claim
// names, so nothing is read through this instance.
const store = new KvClient();

export async function invalidate(key: string) {
  const cached = await store.get(key);
  await store.publish("cache.invalidated", cached ?? key);
}
