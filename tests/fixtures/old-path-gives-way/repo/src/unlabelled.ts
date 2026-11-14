import { plain } from '@fixture/plain';
function send(p: string, b: unknown) { return plain.post(p, b); }
export function f() { return send('/u/a', 1); }
