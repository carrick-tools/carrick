import { http } from '@fixture/http';

export function runPaired(endpoint: string) {
  http.post(`/v1${endpoint}`, { body: null });
  return http.post(`/v2${endpoint}`, { body: null });
}
