export class LedgerFeed {
  private readonly entries: string[] = [];

  publish(topic: string, body: unknown): void {
    this.entries.push(`${topic}:${String(body)}`);
  }
}
