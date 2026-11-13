import { http } from '@fixture/http';
import type { Order } from '../types';

export function placeJson(endpoint: string, data: unknown) {
  return http.post<Order>(`/v1${endpoint}`, JSON.stringify(data));
}
