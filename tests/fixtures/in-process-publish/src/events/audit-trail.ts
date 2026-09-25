export class AuditTrail {
  private readonly lines: string[] = [];

  record(topic: string, body: unknown): void {
    this.lines.push(topic);
    void body;
  }
}
