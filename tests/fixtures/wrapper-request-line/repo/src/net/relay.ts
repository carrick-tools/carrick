import { http } from '@fixture/http';

export function relay(endpoint: string) {
  return http.post(`/v1${endpoint}`, { body: null });
}
