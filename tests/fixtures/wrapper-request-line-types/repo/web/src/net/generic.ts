import { http } from '@fixture/http';
import type { Order } from '../types';

export function placeGeneric<T>(endpoint: string, item: T) {
  return http.post<Order>(`/v1${endpoint}`, { body: item });
}
