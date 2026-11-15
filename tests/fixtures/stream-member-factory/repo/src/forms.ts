import { Channel, Hub } from "fixture-hub";

// (a) A factory returns the member call; the caller states the name.
function open(name: string) {
  return new Hub().channel(name);
}

export async function sendOrders() {
  await open("orders").publish(new Uint8Array());
}

// (b) An async factory, awaited.
async function openAsync(name: string) {
  const hub = new Hub();
  return hub.channel(name);
}

export async function sendAudit() {
  const ch = await openAsync("audit");
  await ch.publish(new Uint8Array());
}

// (c) Held locals.
export async function sendHeld() {
  const hub = new Hub();
  const ch = hub.channel("held");
  await ch.publish(new Uint8Array());
}

// (d) A module-scope constant.
const shared = new Hub().channel("shared");

export async function sendShared() {
  await shared.publish(new Uint8Array());
}

// (e) A runtime value at the name.
async function fromHeaders(api: { init(): Promise<{ headers?: Record<string, string> }> }) {
  const r = await api.init();
  const headers = r.headers ?? {};
  return new Hub().channel(headers["x-channel"]);
}

export async function sendFromHeaders(api: { init(): Promise<{ headers?: Record<string, string> }> }) {
  const ch = await fromHeaders(api);
  await ch.publish(new Uint8Array());
}

// (f) Negative: an in-repo class with a member of the same name.
class LocalHub {
  channel(name: string) {
    return new Channel(name);
  }
}

export async function sendLocal() {
  await new LocalHub().channel("local").publish(new Uint8Array());
}

// (g) The in-place chain.
export async function sendChained() {
  await new Hub().channel("chained").publish(new Uint8Array());
}
