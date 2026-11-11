import { get, getV2, post, postV2 } from './client/options';
export const a = () => get('/a');
export const b = () => getV2('/b');
export const c = () => post('/c', { body: 1 });
export const d = () => postV2('/d', { body: 2 });
