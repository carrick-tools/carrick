import { send } from '@fixture/send';
const HS = 'https://h.test'
type Fetchish = (url: string, init?: { method?: string; headers?: Record<string, string>; body?: string }) => Promise<{ status: number; ok: boolean; text(): Promise<string> }>
export interface Opts { token: string; send?: Fetchish }
export interface Req {
  <T>(method: string, path: string, body?: unknown): Promise<T>
  <T>(method: string, path: string, body: unknown, opts: { on404: 'null' }): Promise<T | null>
}
export function makeHttp(options: Opts): Req {
  const doSend = options.send ?? (send as unknown as Fetchish)
  async function request(method: string, path: string, body?: unknown, opts?: { on404: 'null' }): Promise<unknown> {
    for (let attempt = 0; attempt < 5; attempt++) {
      const response = await doSend(`${HS}${path}`, { method, headers: { 'Authorization': `Bearer ${options.token}` }, body: body === undefined ? undefined : JSON.stringify(body) })
      if (response.status === 429) continue
      const text = await response.text()
      if (response.status === 404 && opts?.on404 === 'null') return null
      return text ? JSON.parse(text) : null
    }
    throw new Error('429')
  }
  return request as Req
}
export function makeReader(opts: Opts) {
  const request = makeHttp(opts)
  async function one(id: string): Promise<unknown> {
    return request<unknown>('GET', `/p/t-real/deals/${id}?x=1`, undefined, { on404: 'null' })
  }
  return { one }
}
export function useIt() { return makeReader({ token: 't' }).one('7') }
