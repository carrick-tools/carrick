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
