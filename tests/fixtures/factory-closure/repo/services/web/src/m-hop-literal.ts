function make() {
  async function request(method: string, path: string) {
    return fetch(`https://h.test${path}`, { method: 'PUT' });
  }
  return request;
}
export function put() { const hs = make(); return hs('POST', '/p/m-hop-literal'); }
