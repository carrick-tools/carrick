import { Cache } from "./cache.js";

interface ClientConfig {
  apiEndpoint: string;
}

export class ApiClient {
  private gatewayUrl: string;
  private cache: Cache;

  constructor(config: ClientConfig) {
    // The whole URL is assembled here and held in a field, so no request in
    // this file states a route-shaped argument of its own.
    this.gatewayUrl = `${config.apiEndpoint}/rpc/gateway`;
    this.cache = new Cache();
  }

  async searchByIntent(query: string): Promise<unknown> {
    const response = await fetch(this.gatewayUrl, {
      method: "POST",
      body: JSON.stringify({ action: "search-by-intent", query }),
    });
    return response.json();
  }

  async getAllRepoData(): Promise<unknown[]> {
    // The delegation goes through a callback the cache invokes. The cache read
    // is a verb-named member call with no string argument, which is not a
    // request.
    return this.cache.get(() => this.fetchCrossRepoData());
  }

  async findService(name: string): Promise<unknown> {
    const repos = await this.getAllRepoData();
    return repos.find((repo) => (repo as { name: string }).name === name);
  }

  async refreshEverything(): Promise<void> {
    // Two requests, two actions: a site calling this could be reaching either,
    // so nothing is carried onto it.
    await fetch(this.gatewayUrl, {
      method: "POST",
      body: JSON.stringify({ action: "invalidate-cache" }),
    });
    await fetch(this.gatewayUrl, {
      method: "POST",
      body: JSON.stringify({ action: "rebuild-index" }),
    });
  }

  private async fetchCrossRepoData(): Promise<unknown[]> {
    const response = await fetch(this.gatewayUrl, {
      method: "POST",
      body: JSON.stringify({ action: "get-cross-repo-data" }),
    });
    const data = (await response.json()) as {
      staged_url?: string;
      repos: unknown[];
    };
    if (data.staged_url) {
      // A bare read of a presigned URL: no options bag and no verb property,
      // so it is not a second request and the member still states one.
      const staged = await fetch(data.staged_url);
      return ((await staged.json()) as { repos: unknown[] }).repos;
    }
    return data.repos;
  }
}
