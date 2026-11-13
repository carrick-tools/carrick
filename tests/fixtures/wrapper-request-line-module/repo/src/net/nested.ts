import { http } from '@fixture/http';

export function sendNested(endpoint: string) {
  return http.post(`/v1${endpoint}`, { body: null });
}
