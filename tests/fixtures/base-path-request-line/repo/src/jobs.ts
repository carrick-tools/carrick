import { sendConst } from './net/constbase';
import { sendStop } from './net/stopbase';

export function postTotals() {
  return sendConst('/totals');
}
export function postRead(read: () => string) {
  return sendStop(read());
}
