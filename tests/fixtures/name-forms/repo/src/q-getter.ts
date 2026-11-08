import { bus } from '@fixture/queue';
class T { get topic() { return 'orders.getter'; } async go() { await bus.publish(this.topic, { a: 1 }); } }
export async function f() { await new T().go(); }
