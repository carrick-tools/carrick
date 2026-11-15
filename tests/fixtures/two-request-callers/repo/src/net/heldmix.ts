import { http } from '@fixture/http';

export function runHeldMix(endpoint: string) {
  http.request(`/v3${endpoint}`, { method: 'GET' });
  return http.post('/v3/settle', { body: null });
}
