import { http } from '@fixture/http';

export function sendOpt(endpoint: string) {
  return http.post(`/v1${endpoint}`, { body: null });
}
