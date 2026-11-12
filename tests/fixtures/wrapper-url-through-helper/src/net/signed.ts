declare const session: { key?: string } | undefined

const ROOT = '/api/v1'

function signed(url: string): string {
  let out = url
  if (session?.key) out = `${url}?sig=${session.key}`
  return out
}

export async function signedFetch<T>(path: string, options?: RequestInit): Promise<T> {
  const res = await fetch(signed(`${ROOT}${path}`), { ...options })
  return (await res.json()) as T
}
