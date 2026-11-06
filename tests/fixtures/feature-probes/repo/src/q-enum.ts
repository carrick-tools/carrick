import { bus } from '@fixture/queue';
enum T { A = 'q-enum' }
export async function f() { await bus.publish(T.A, { a: 1 }); }
