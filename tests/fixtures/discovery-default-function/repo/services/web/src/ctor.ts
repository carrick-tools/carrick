type Send = (url: string, init?: RequestInit) => Promise<Response>;

export class Mailer {
  constructor(
    private base: string,
    private send: Send = (u, i) => fetch(u, i),
  ) {}

  async deliver(): Promise<unknown> {
    const res = await this.send(`${this.base}/v1/mail`, { method: "POST" });
    return res.json();
  }
}
