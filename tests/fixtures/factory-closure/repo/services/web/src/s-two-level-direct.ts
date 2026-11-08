import { send } from '@fixture/send';
export interface Req { <T>(method: string, path: string, body?: unknown): Promise<T> }
export function makeHttp(options: { token: string }): Req {
  async function request(method: string, path: string, body?: unknown): Promise<unknown> {
    return send(`/p${path}`, body)
  }
  return request as Req
}
export function makeClient(opts: { token: string }) {
  const hs = makeHttp(opts)
  async function search(): Promise<unknown> { return hs<unknown>('POST', '/s-two-level-direct', {}) }
  return { search }
}
export function useIt() { return makeClient({ token: 't' }).search() }
