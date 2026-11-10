import { http } from '@fixture/http';

export function save(path: string, body: unknown) {
  return http.post(path, body);
}
