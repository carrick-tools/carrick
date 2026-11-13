export interface SiteConnection {
  url: string
  token: string
  fetchImpl?: typeof fetch
}

interface Note {
  id: number
  slug: string
}

function normalizeBase(url: string): string {
  return url.replace(/\/$/, '')
}

function resolveSite(connection: SiteConnection): { siteUrl: string } {
  return { siteUrl: normalizeBase(connection.url) }
}

async function sendJson<T>(
  connection: SiteConnection,
  siteUrl: string,
  path: string,
  init?: RequestInit,
): Promise<{ body: T; response: Response }> {
  const response = await (connection.fetchImpl ?? fetch)(`${normalizeBase(siteUrl)}${path}`, {
    ...init,
    headers: { Authorization: `Bearer ${connection.token}`, ...(init?.headers ?? {}) },
  })
  return { body: (await response.json()) as T, response }
}

export async function createNote(
  connection: SiteConnection,
  draft: { title: string; slug: string },
): Promise<Note> {
  const site = resolveSite(connection)
  const { body } = await sendJson<Note>(connection, site.siteUrl, '/api/notes', {
    method: 'POST',
    body: JSON.stringify({ title: draft.title, slug: draft.slug }),
  })
  return body
}

export async function renameNote(connection: SiteConnection, id: number, title: string): Promise<Note> {
  const site = resolveSite(connection)
  const { body } = await sendJson<Note>(connection, site.siteUrl, `/api/notes/${id}`, {
    method: 'POST',
    body: JSON.stringify({ title }),
  })
  return body
}
