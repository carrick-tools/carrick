import { reorder } from './net/reorder';
export const warm = reorder('/orders');
export const again = () => reorder('/orders');
