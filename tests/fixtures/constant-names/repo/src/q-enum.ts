import { bus } from '@fixture/queue';
import { Topic } from './names';
export async function f() { await bus.publish(Topic.Created, { a: 1 }); }
