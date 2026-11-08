import { bus } from '@fixture/queue';
const { topic } = { topic: 'orders.destructured' };
export async function f() { await bus.publish(topic, { a: 1 }); }
