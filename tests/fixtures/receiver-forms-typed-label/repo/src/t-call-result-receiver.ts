import { http } from '@fixture/http';
function getClient() { return http; }
export function f() { return getClient().post('/p/t-call-result-receiver', null); }
