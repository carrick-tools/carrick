import { mergeOptions, send } from "./send.js";

const ArtifactSchema = { kind: "upload" };

export class ApiClient {
  constructor(
    private readonly baseUrl: string,
    private readonly defaults: { retries?: number } = {}
  ) {}

  createArtifactUrl(filename: string) {
    const encoded = encodeURIComponent(filename);
    return send(
      ArtifactSchema,
      `${this.baseUrl}/api/v2/artifacts/${encoded}`,
      {
        method: "PUT",
        headers: this.headers(),
      },
      mergeOptions(this.defaults, {})
    );
  }

  readArtifactUrl(filename: string) {
    const encoded = encodeURIComponent(filename);
    return send(
      ArtifactSchema,
      `${this.baseUrl}/api/v1/artifacts/${encoded}`,
      {
        method: "GET",
        headers: this.headers(),
      },
      mergeOptions(this.defaults, {})
    );
  }

  private headers() {
    return { Accept: "application/json" };
  }
}
