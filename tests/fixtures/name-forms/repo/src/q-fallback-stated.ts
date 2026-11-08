import { bus } from '@fixture/queue';
const topic = 'orders.fallback-stated';
bus.subscribe(topic ?? 'orders.fallback-stated', async (_p) => {});
