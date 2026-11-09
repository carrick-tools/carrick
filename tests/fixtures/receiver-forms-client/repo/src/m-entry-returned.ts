import { deps } from './m-entry-deps';
function pick() { return deps.client; }
export function f() { return pick().get('/p/m-entry-returned'); }
