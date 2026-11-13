import { restock } from './net/restock';
export const any = restock(process.argv[2]);
export const known = restock('/orders');
