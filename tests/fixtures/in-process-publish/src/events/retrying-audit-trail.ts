import { AuditTrail } from "./audit-trail";
import { withRetry } from "./with-retry";

export class RetryingAuditTrail extends withRetry(AuditTrail) {
  override record(topic: string, body: unknown): void {
    void fetch(`/audit/${topic}`, { method: "POST", body: JSON.stringify(body) });
  }
}
