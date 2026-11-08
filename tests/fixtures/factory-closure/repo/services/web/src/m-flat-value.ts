async function request(method: string, path: string) {
  return fetch(`https://h.test${path}`, { method });
}
export function post() { const hs = request; return hs('POST', '/p/m-flat-value'); }
