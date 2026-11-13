import { http } from '@fixture/http';
import type { Order } from '../types';

export function placeLoose(endpoint: string, body: unknown) {
  return http.post<Order>(`/v1${endpoint}`, { body });
}
