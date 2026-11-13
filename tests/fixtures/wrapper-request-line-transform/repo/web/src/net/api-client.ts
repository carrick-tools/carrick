export class ApiClient {
  constructor(private readonly originUrl: string) {}

  async streamPost(path: string, body: unknown): Promise<Response> {
    const url = `${this.originUrl}/v1${path}`;
    let res: Response;
    try {
      res = await fetch(url, { method: 'POST', body: JSON.stringify(body ?? {}) });
    } catch (err) {
      throw new Error(`stream failed: ${String(err)}`);
    }
    if (!res.ok) throw new Error(`stream failed: ${res.status}`);
    return res;
  }
}

export function createApiClient(): ApiClient {
  return new ApiClient('');
}
