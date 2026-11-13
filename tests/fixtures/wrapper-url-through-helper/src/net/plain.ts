const ROOT = '/api/v1'

function passThrough(url: string): string {
  return url
}

export async function plainFetch<T>(path: string, options?: RequestInit): Promise<T> {
  const res = await fetch(`${ROOT}${path}`, { ...options })
  return (await res.json()) as T
}

export async function passFetch<T>(path: string, options?: RequestInit): Promise<T> {
  const res = await fetch(passThrough(`${ROOT}${path}`), { ...options })
  return (await res.json()) as T
}

function hostFor(region: string): string {
  let host = 'https://api.example.com'
  if (region) host = `https://${region}.example.com`
  return host
}

export async function regionFetch<T>(region: string): Promise<T> {
  const res = await fetch(`${hostFor(region)}/badges`)
  return (await res.json()) as T
}

export async function badgeById<T>(id: string): Promise<T> {
  const res = await fetch(`/api/v1/badges/${encodeURIComponent(id)}`)
  return (await res.json()) as T
}
