import { bus } from '@fixture/queue';
function emit(t: string, p: unknown) { return bus.publish(t, p); }
export async function f() { await emit('q-wrapper', { a: 1 }); }
