import { http } from '@fixture/http';

export function openSession(endpoint: string) {
  http.post('/v1/token', { body: null });
  return http.post(`/v1${endpoint}`, { body: null });
}
