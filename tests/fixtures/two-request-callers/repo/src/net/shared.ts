import { http } from '@fixture/http';

export function runShared(endpoint: string) {
  http.post(`/v1${endpoint}`, { body: null });
  return http.post('/v1/close', { body: null });
}
