// A service that resolves every path against the server it was built with.
export interface Server {
  baseUrl: string;
}

export class FetchService {
  constructor(private readonly server: Server) {}

  async fetch(input: string, init?: RequestInit) {
    return globalThis.fetch(new URL(input, this.server.baseUrl), init);
  }
}
