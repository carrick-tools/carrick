import { http } from '@fixture/http';

export function track(body: unknown) {
  return http.post('/track', body);
}
