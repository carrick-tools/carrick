import { bus } from '@fixture/queue';
async function emit(topic = 'orders.defaulted') { await bus.publish(topic, { a: 1 }); }
export function f() { return emit(); }
