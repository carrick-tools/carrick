import { http } from '@fixture/http';
namespace NS { export const P = '/p/n-namespace'; }
export function f() { return http.post(NS.P, null); }
