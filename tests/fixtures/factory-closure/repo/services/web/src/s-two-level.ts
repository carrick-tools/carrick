import { send } from '@fixture/send';
type Send = (url: string, body?: unknown) => Promise<unknown>;
export interface Req { <T>(method: string, path: string, body?: unknown): Promise<T> }
export function makeHttp(options: { token: string; send?: Send }): Req {
  const doSend = options.send ?? send
  async function request(method: string, path: string, body?: unknown): Promise<unknown> {
    for (let attempt = 0; attempt < 5; attempt++) {
      const response = await doSend(`/p${path}`, body)
      if (response) return response
    }
    throw new Error('x')
  }
  return request as Req
}
export function makeClient(opts: { token: string }) {
  const hs = makeHttp(opts)
  async function search(): Promise<unknown> { return hs<unknown>('POST', '/s-two-level', {}) }
  return { search }
}
export function useIt() { return makeClient({ token: 't' }).search() }
