import { http } from '@fixture/http';

export function sendBoot(endpoint: string) {
  return http.post(`/v1${endpoint}`, { body: null });
}
