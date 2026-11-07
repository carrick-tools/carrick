import { http, Client } from '@fixture/http';
const c: Client | undefined = flagOn() ? http : undefined;
function flagOn() { return true; }
export function f() { return c?.post('/p/t-optional-chain', null); }
