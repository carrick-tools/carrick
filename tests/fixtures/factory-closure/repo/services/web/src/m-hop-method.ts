function make() {
  async function request(method: string, path: string) {
    return fetch(`https://h.test${path}`, { method });
  }
  return request;
}
export function post() { const hs = make(); return hs('POST', '/p/m-hop-post'); }
export function get() { const hs = make(); return hs('GET', '/p/m-hop-get'); }
