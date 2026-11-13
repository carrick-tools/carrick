import { http } from '@fixture/http';

export function ping(endpoint: string) {
  return http.post(`/v1${endpoint}`, { body: null });
}
