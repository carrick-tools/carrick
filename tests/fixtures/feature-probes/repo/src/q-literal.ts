import { bus } from '@fixture/queue';
export async function f() { await bus.publish('q-literal', { a: 1 }); }
