import { bus } from '@fixture/queue';
class T { static TOPIC = 'orders.static-field'; }
export async function f() { await bus.publish(T.TOPIC, { a: 1 }); }
