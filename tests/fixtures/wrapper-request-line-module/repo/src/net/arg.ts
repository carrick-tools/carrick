import { http } from '@fixture/http';

export function sendArg(endpoint: string) {
  return http.post(`/v1${endpoint}`, { body: null });
}
