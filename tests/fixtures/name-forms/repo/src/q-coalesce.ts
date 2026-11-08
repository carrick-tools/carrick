import { bus } from '@fixture/queue';
declare const maybe: string | undefined;
bus.subscribe(maybe ?? 'orders.coalesced', async (_p) => {});
