import { http } from '@fixture/http';
export function f() {
  const { path } = { path: '/c/destructure-local' };
  return http.post(path, null);
}
