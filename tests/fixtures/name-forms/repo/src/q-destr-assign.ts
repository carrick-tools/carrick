import { bus } from '@fixture/queue';
let topic = '';
({ topic } = { topic: 'orders.destr-assign' });
export async function f() { await bus.publish(topic, { a: 1 }); }
