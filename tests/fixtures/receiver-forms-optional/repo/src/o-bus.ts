import { bus } from '@fixture/queue';
export function f() { return bus?.publish('t.o-bus', {}); }
