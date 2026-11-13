import { http } from '@fixture/http';

export function sendRight(endpoint: string) {
  return http.post(`/v1${endpoint}`, { body: null });
}
