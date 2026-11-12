import { placeOrder } from './net/orders';
import { sendRefund } from './net/refunds';

export const buyBook = () => placeOrder('/orders');
export const buyPen = () => placeOrder('/orders');
export const refund = () => sendRefund('/refunds');
