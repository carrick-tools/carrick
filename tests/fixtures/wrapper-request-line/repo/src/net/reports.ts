import { http } from '@fixture/http';

export function report(endpoint: string) {
  return http.post(`/v1${endpoint}`, { body: null });
}
