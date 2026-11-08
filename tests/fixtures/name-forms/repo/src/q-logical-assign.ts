import { bus } from '@fixture/queue';
let topic: string | undefined;
export function f() { topic ??= 'orders.logical-assign'; bus.subscribe(topic, async (_p) => {}); }
