type Payload = Record<string, unknown>;

export class PortalClient {
  private base: string;
  private token: string | undefined;

  constructor(host: string, token?: string) {
    this.base = host;
    this.token = token;
  }

  // The URL is written inline in the object the wrapper is handed.
  public async openSession(kind: string, ref: string | null) {
    return await this.submit({
      target: this.base + `/sessions/open/${kind}${this.query(ref)}`,
      payload: { kind },
    });
  }

  // The URL is held in a constant first.
  public async closeSession(kind: string, ref: string | null) {
    const target = this.base + `/sessions/close/${kind}${this.query(ref)}`;
    const res = await this.submit({ target });
    return res;
  }

  // No query at all.
  public async listSessions() {
    return await this.submit({ target: `${this.base}/sessions` });
  }

  // A function between the caller and the wrapper passes its own options on.
  public async renewSession(kind: string) {
    return await this.submitTraced({ target: `${this.base}/sessions/renew/${kind}` });
  }

  private async submitTraced(options: { target: string; payload?: Payload }) {
    console.log("submitting");
    return await this.submit(options);
  }

  private async submit({ target, payload }: { target: string; payload?: Payload }) {
    const res = await fetch(target, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      ...(payload ? { body: JSON.stringify(payload) } : {}),
    });
    return res.json();
  }

  private query(ref: string | null): string {
    const parts: string[] = [];
    if (ref) {
      parts.push(`ref=${ref}`);
    }
    if (this.token) {
      parts.push(`token=${this.token}`);
    }
    return parts.length === 0 ? "" : "?" + parts.join("&");
  }
}
