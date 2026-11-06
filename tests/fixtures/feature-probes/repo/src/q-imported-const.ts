import { bus } from '@fixture/queue';
import { TOPIC } from './consts/q-topic';
export async function f() { await bus.publish(TOPIC, { a: 1 }); }
