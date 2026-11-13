import { placeOrder } from './net/orders';
import { sendRefund } from './net/refunds';
import { openSession } from './net/session';

export const buyBook = () => placeOrder('/orders');
export const buyPen = () => placeOrder('/orders');
export const refund = () => sendRefund('/refunds');
export const start = () => openSession('/sessions');
