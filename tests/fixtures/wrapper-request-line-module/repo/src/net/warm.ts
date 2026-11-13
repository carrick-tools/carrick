import { http } from '@fixture/http';

export function sendWarm(endpoint: string) {
  return http.post(`/v1${endpoint}`, { body: null });
}
