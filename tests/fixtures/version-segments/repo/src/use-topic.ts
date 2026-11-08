import { publish } from './client/topic';
export const t1 = () => publish({ a: 1 });
export const t2 = () => publish({ a: 2 }, { topic: 'orders.given' });
