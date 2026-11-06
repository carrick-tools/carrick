import { bus } from '@fixture/queue';
const T = { S: 'q-subscribe' } as const;
bus.subscribe(T.S, async (_p) => {});
