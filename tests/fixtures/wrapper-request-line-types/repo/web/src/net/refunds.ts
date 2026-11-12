import { http } from '@fixture/http';
import type { Refund } from '../types';

export function sendRefund(endpoint: string) {
  return http.post<Refund>(`/v1${endpoint}`, { body: 5 });
}
