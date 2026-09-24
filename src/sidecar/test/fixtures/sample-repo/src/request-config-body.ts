// carrick-cloud#1366 shapes 2 and 3: consumer calls through a client whose
// DELETE takes a request CONFIG (the body rides on its `data` member), and
// whose POST names its response with a type argument. Synthetic client.

export interface RequestConfig<D = unknown> {
  headers?: Record<string, string>;
  params?: Record<string, string>;
  data?: D;
  timeout?: number;
}

export interface ClientResponse<T> {
  data: T;
  status: number;
}

export interface HttpClient {
  post<T = unknown, D = unknown>(
    url: string,
    data?: D,
    config?: RequestConfig<D>
  ): Promise<ClientResponse<T>>;
  delete<T = unknown, D = unknown>(
    url: string,
    config?: RequestConfig<D>
  ): Promise<ClientResponse<T>>;
}

declare const api: HttpClient;

export interface QueryResultData {
  rows: Array<Record<string, unknown>>;
  rowCount: number;
}

export async function removeAccount(id: string, reason: string): Promise<void> {
  await api.delete(`/accounts/${id}`, { data: { reason } });
}

export async function removeAccountQuietly(id: string): Promise<void> {
  await api.delete(`/accounts/${id}`, { params: { quiet: 'true' } });
}

export async function sendEnvelope(): Promise<void> {
  await api.post('/envelopes', { data: 1, label: 'x' });
}

export async function runQuery(sql: string): Promise<QueryResultData> {
  const response = await api.post<QueryResultData>('/ops/query', { sql });
  return response.data;
}

export async function authenticate(masterKey: string): Promise<void> {
  await api.post('/ops/auth', { masterKey });
}

export async function authenticateWithPin(pin: number): Promise<void> {
  await api.post('/ops/auth', { masterKey: pin });
}

export async function removeAccountWithCode(id: string, code: number): Promise<void> {
  await api.delete(`/accounts/${id}`, { data: { reasonCode: code } });
}

export async function runQueryMisnamed(sql: string): Promise<QueryResultData> {
  const response = await api.post<QueryResultData>('/ops/query', { query: sql });
  return response.data;
}

// Review follow-ups: a config held in a variable, a client called through its
// static default, and three option shapes whose generic member is not a body.

export async function removeWithBuiltConfig(id: string, reason: string): Promise<void> {
  const removal = { data: { reason } };
  await api.delete(`/accounts/${id}`, removal);
}

export async function removeWithTypedConfig(id: string, opaque: RequestConfig): Promise<void> {
  await api.delete(`/accounts/${id}`, opaque);
}

export interface HttpStatic extends HttpClient {
  <T = unknown>(config: RequestConfig): Promise<ClientResponse<T>>;
  create(config?: RequestConfig): HttpClient;
}

declare const httpDefault: HttpStatic;

export async function removeThroughStatic(id: string, reason: string): Promise<void> {
  await httpDefault.delete(`/accounts/${id}`, { data: { reason, via: 'static' } });
}

export type ResponseMode = 'json' | 'text' | 'blob';

export interface FetchOptions<R extends ResponseMode = 'json'> {
  method?: string;
  body?: Record<string, unknown> | string;
  headers?: Record<string, string>;
  responseType?: R;
}

declare function fetchData<T = unknown, R extends ResponseMode = 'json'>(
  url: string,
  options?: FetchOptions<R>
): Promise<T>;

export async function saveNote(note: { text: string }): Promise<void> {
  await fetchData('/notes', { body: note, responseType: 'json' });
  await fetchData('/notes/raw', { method: 'POST', body: note });
}

export interface JsonOptions {
  json?: unknown;
  searchParams?: Record<string, string>;
}

declare const shortClient: { post(url: string, options?: JsonOptions): Promise<unknown> };
declare const streamClient: {
  post(url: string, options?: JsonOptions & { responseType?: 'json' }): Promise<unknown>;
};

export async function saveViaShort(note: { text: string }): Promise<void> {
  await shortClient.post('/notes/short', { json: note });
}

export async function saveViaStream(note: { text: string }): Promise<void> {
  await streamClient.post('/notes/stream', { json: note, responseType: 'json' });
}
