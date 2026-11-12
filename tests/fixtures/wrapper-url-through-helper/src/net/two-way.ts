declare const settings: { ticket?: string } | undefined

const ROOT = '/api/v1'

function maybeTicket(url: string): string {
  if (!settings?.ticket) return url
  return `${url}?ticket=${settings.ticket}`
}

export async function twoReturnFetch<T>(path: string, options?: RequestInit): Promise<T> {
  const res = await fetch(maybeTicket(`${ROOT}${path}`), { ...options })
  return (await res.json()) as T
}
