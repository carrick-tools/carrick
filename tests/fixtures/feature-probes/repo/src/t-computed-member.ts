import { http } from '@fixture/http';
const k = 'post' as const;
export function f() { return http[k]('/p/t-computed-member', null); }
