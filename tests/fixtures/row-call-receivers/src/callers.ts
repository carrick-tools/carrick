import { Client } from "./client.js";
import { Context, type Deps } from "./context.js";
import type { Store } from "./store.js";

export async function viaMethodFactory(ctx: Context) {
  const c = await ctx.getClient();
  c.getUser();
}

export class Worker extends Context {
  async viaThisMethodFactory() {
    const c = await this.getClient();
    c.runQuery();
  }
}

export function viaPropertyChain(holder: { client: Client }) {
  holder.client.notify();
}

export class Chained {
  constructor(private options: { client: Client }) {}

  viaThisChain() {
    this.options.client.enqueue();
  }
}

export function viaDestructured({ client }: Deps) {
  client.callProcedure();
}

export const viaDestructuredUntyped = (input: unknown, { ctx }: { ctx: Context }) =>
  ctx.getClient().then((c) => c.getUser());

let current: Client | null = null;

export function viaUnion() {
  current!.runQuery();
}

export function viaOptional() {
  current?.notify();
}

export const viaUnannotatedParam = [new Client()].map((client) => client.enqueue());

export function viaInterface(store: Store) {
  store.read();
}

export function viaAny(c: any) {
  c.getUser();
}

export function viaLibrary() {
  const xs: number[] = [1, 2];
  xs.map((x) => x + 1);
}
