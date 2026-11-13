import { postOrder } from './net/orders';
export const place = () => postOrder('/orders');
export const cancel = () => postOrder('/orders/cancel');
