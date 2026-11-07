import { bus } from '@fixture/queue';
const NS = 'q-template';
export async function f() { await bus.publish(`${NS}.created`, { a: 1 }); }
