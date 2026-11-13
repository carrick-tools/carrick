import { http } from '@fixture/http';

export function sendLeft(endpoint: string) {
  return http.post(`/v1${endpoint}`, { body: null });
}
