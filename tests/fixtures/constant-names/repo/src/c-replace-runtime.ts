import { http } from '@fixture/http';
export function f(id: string) { return http.post('/c/replace-runtime/:id'.replace(':id', id), null); }
