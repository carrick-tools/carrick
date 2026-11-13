declare const session: { key?: string } | undefined

const ROOT = '/api/v1'

function stamped(url: string): string {
  let out = url
  if (session?.key) out = `${url}?stamp=${session.key}`
  return out
}

export async function stampedFetch<T>(path: string): Promise<T> {
  const res = await fetch(stamped(`${ROOT}${path}`))
  return (await res.json()) as T
}

export async function fixedStamp(): Promise<unknown> {
  const res = await fetch(stamped('/fixed'))
  return res.json()
}
