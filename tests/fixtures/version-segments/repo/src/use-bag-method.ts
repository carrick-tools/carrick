import { put, patch } from './client/bag-method';
export const u1 = () => put('/one');
export const u2 = () => patch('/two');
