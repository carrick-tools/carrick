import { http } from '@fixture/http';

export function notify(endpoint: string) {
  return http.post(`/v1${endpoint}`, { body: null });
}
