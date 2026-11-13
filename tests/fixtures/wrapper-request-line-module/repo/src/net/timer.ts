import { http } from '@fixture/http';

export function sendTimer(endpoint: string) {
  return http.post(`/v1${endpoint}`, { body: null });
}
