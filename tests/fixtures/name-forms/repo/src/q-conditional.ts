import { bus } from '@fixture/queue';
declare const flag: boolean;
bus.subscribe(flag ? 'orders.conditional-a' : 'orders.conditional-b', async (_p) => {});
