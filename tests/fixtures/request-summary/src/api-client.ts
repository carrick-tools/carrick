import { Cache } from "./cache.js";
import { matchService } from "./match.js";
import { Poller } from "./poller.js";

/**
 * How the client reaches the gateway.
 *
 * The gateway is one RPC-style route that dispatches on the body's `action`
 * field, so every request below goes to the same URL and names its operation
 * in the body. The URL is assembled once, in the constructor, from the
 * endpoint the deployment injects, and held in a field: no request in this
 * file states a route-shaped argument of its own.
 *
 * This file is deliberately longer than four kilobytes. The model is shown a
 * wrapper module's source cut at that length, so everything past the
 * constructor — every member body, every request, every action literal — is
 * invisible to it, which is the production shape this fixture reproduces.
 */
export interface ApiClientConfig {
  /** The deployment's API origin, e.g. `https://api.example.test`. */
  apiEndpoint: string;
  /** Sent as a bearer token on every request. */
  apiKey: string;
}

/** One search hit, as the gateway returns it. */
export interface SearchHit {
  name: string;
  file: string;
  similarity: number;
}

/** What a job-status read answers. `null` when the gateway has no job. */
export interface JobStatus {
  state: "queued" | "running" | "done" | "failed";
  progress: number;
}

/**
 * The client every tool is handed.
 *
 * Tools receive it as a parameter and never construct it, so no tool file
 * imports this module for anything but its type. A reader of a tool file sees
 * `client.getAllRepoData()` and nothing else: no URL, no method, no action.
 * Everything a contract check needs about that call is written here, one to
 * three calls away from the line that sends it.
 *
 * The cache is shared across the tools of one request. `invalidateCache`
 * clears it and sends nothing; a reader that takes its name for a request
 * reads a request into a name.
 */
export class ApiClient {
  private lambdaUrl: string;
  private apiKey: string;
  private cache: Cache;

  constructor(config: ApiClientConfig) {
    this.lambdaUrl = `${config.apiEndpoint}/types/check-or-upload`;
    this.apiKey = config.apiKey;
    this.cache = new Cache();
  }

  /**
   * Every repo the project indexes. Read through the cache: the request is
   * made by the producer handed to it, which the cache invokes with the
   * digest it holds.
   */
  async getAllRepoData(): Promise<unknown[]> {
    return this.cache.get((knownDigest) => this.fetchCrossRepoData(knownDigest));
  }

  /** The repo a service name names: one delegation further from the request. */
  async findService(name: string): Promise<unknown> {
    const repos = await this.getAllRepoData();
    return matchService(repos, name);
  }

  /** Drop the cached copy. Sends nothing. */
  invalidateCache(): void {
    this.cache.invalidate();
  }

  /**
   * Start polling. Nothing here is a call, and a constructor can still send
   * a request, so this is never proven to send nothing.
   */
  startPolling(): void {
    new Poller(this.lambdaUrl);
  }

  /** Server-side search. Not cached: the query is the key. */
  async searchByIntent(query: string): Promise<SearchHit[]> {
    const response = await fetch(this.lambdaUrl, {
      method: "POST",
      headers: this.headers(),
      body: JSON.stringify({ action: "search-by-intent", query }),
    });
    return (await response.json()) as SearchHit[];
  }

  /** Functions described like the ones given. The params ride in the body. */
  async findSimilar(params: { names: string[] }): Promise<SearchHit[]> {
    const response = await fetch(this.lambdaUrl, {
      method: "POST",
      headers: this.headers(),
      body: JSON.stringify({ action: "find-similar", ...params }),
    });
    return (await response.json()) as SearchHit[];
  }

  /** Where one repo's analysis job is. */
  async analysisJobStatus(repo: string): Promise<JobStatus | null> {
    const response = await fetch(this.lambdaUrl, {
      method: "POST",
      headers: this.headers(),
      body: JSON.stringify({ action: "analysis-job-status", repo }),
    });
    if (response.status === 404) return null;
    return (await response.json()) as JobStatus;
  }

  /** The projects this key can see, through the shared RPC helper. */
  async listProjects(): Promise<unknown[]> {
    return (await this.rpc({ action: "list-projects" })) as unknown[];
  }

  /** One RPC call: the body is the caller's, serialised as it arrives. */
  private async rpc(body: Record<string, unknown>): Promise<unknown> {
    const response = await fetch(this.lambdaUrl, {
      method: "POST",
      headers: this.headers(),
      body: JSON.stringify(body),
    });
    return response.json();
  }

  private headers(): Record<string, string> {
    return {
      "Content-Type": "application/json",
      Authorization: `Bearer ${this.apiKey}`,
    };
  }

  private async fetchCrossRepoData(
    knownDigest: string | null,
  ): Promise<{ data: unknown[]; digest: string | null }> {
    const response = await fetch(this.lambdaUrl, {
      method: "POST",
      headers: this.headers(),
      body: JSON.stringify({
        action: "get-cross-repo-data",
        known_digest: knownDigest,
      }),
    });
    const body = (await response.json()) as {
      staged_url?: string;
      repos: unknown[];
      digest: string | null;
    };
    if (body.staged_url) {
      // A presigned read: a request whose URL the gateway supplies at run
      // time, so nothing here can state where it goes.
      const staged = await fetch(body.staged_url);
      return { data: (await staged.json()) as unknown[], digest: body.digest };
    }
    return { data: body.repos, digest: body.digest };
  }
}
