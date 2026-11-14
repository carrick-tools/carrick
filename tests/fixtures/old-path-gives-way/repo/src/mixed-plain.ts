import { http } from '@fixture/http';
import { plain } from '@fixture/plain';
function send(p: string, b: unknown) { return http.post(p, b); }
function sendPlain(p: string, b: unknown) { return plain.post(p, b); }
export function f() { return [send('/q/a', 1), sendPlain('/q/b', 2)]; }
