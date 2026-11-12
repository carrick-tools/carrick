import { http } from '@fixture/http';

export function charge(path: string) {
  return http.post(`/billing${path}`, { body: null });
}
