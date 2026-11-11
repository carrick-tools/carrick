import { send } from './client/method';
export const m1 = () => send('/m/omitted');
export const m2 = () => send('/m/get', { method: 'GET' });
export const m3 = () => send('/m/body', { body: 1 });
