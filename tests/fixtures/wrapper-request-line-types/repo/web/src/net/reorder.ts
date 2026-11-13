import { http } from '@fixture/http';
import type { Order } from '../types';

export function reorder(endpoint: string) {
  return http.post<Order>(`/v1${endpoint}`, { body: 'book' });
}
