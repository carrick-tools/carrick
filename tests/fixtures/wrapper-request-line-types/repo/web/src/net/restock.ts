import { http } from '@fixture/http';
import type { Order } from '../types';

export function restock(endpoint: string) {
  return http.post<Order>(`/v1${endpoint}`, { body: 'book' });
}
