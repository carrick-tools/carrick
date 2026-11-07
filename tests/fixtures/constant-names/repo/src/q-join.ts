import { bus } from '@fixture/queue';
bus.subscribe(['orders', 'shipped'].join('.'), async (_p) => {});
