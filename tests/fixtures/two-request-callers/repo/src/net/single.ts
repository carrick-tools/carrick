import { http } from '@fixture/http';

export function sendSingle(endpoint: string) {
  return http.post(`/v1${endpoint}`, { body: null });
}
