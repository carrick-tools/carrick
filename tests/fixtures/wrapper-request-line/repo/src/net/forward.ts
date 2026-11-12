import { http } from '@fixture/http';

export function forward(endpoint: string) {
  return http.post(`/v1${endpoint}`, { body: null });
}
