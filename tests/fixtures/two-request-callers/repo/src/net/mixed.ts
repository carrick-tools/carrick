import { http } from '@fixture/http';

export function runMixed(endpoint: string) {
  http.request(`/v1${endpoint}`, { method: 'GET' });
  return http.post('/v1/settle', { body: null });
}
