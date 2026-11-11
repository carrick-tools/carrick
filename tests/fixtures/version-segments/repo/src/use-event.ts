import { announce } from './client/event';
export const v1 = () => announce({ a: 1 });
export const v2 = () => announce({ a: 2 }, { event: 'room.given' });
