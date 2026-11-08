function make() {
  async function request(path: string, method?: string) {
    return fetch(`https://h.test${path}`, { method });
  }
  return request;
}
export function get() { const hs = make(); return hs('/p/m-hop-default'); }
