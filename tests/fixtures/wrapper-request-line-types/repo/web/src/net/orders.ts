import { http } from '@fixture/http';
import type { Order } from '../types';

export function placeOrder(endpoint: string) {
  return http.post<Order>(`/v1${endpoint}`, { body: 'book' });
}
