import { http } from '@fixture/http';
import type { Order } from '../types';

export interface Draft { title: string }
export function orderDraft(draft: Draft) {
  const wire = { body: draft.title };
  return http.post<Order>('/v1/orders', wire);
}
export class OrderClient {
  async place(draft: Draft) {
    const wire = { body: draft.title };
    return http.post<Order>('/v1/orders', wire);
  }
}
export function orderAsIs(body: { body: string }) {
  return http.post<Order>('/v1/orders', body);
}
export function orderInline(draft: Draft) {
  return http.post<Order>('/v1/orders', { body: draft.title });
}
