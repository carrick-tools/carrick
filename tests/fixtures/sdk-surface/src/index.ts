import * as API from "./resources/index.js";
import { TransfersApi } from "./api/transfers.js";
import { auditLog } from "./util/audit.js";
import { chargeCard } from "./util/direct.js";

export default class Ledger {
  payments: API.Payments = new API.Payments(this);
  reports: API.Reports = new API.Reports(this);
  private readonly transfersClient: API.Transfers = new API.Transfers(
    new TransfersApi(),
  );

  constructor(
    private readonly baseUrl: string,
    public receipts: API.Refunds = new API.Refunds(),
  ) {}

  // Handed back by an arrow-valued field: `ledger.transfers().send(...)`.
  transfers = () => this.transfersClient;

  // Handed back by a method, named by its declared return type.
  refunds(): API.Refunds {
    return new API.Refunds();
  }

  async scrape(target: string): Promise<unknown> {
    auditLog("scrape", target);
    return fetch(`${this.baseUrl}/v1/scrape`, {
      method: "POST",
      body: JSON.stringify({ target }),
    });
  }

  get baseHost(): string {
    return this.baseUrl;
  }
}

export const admin = {
  settings,
  chargeCard,
  cancel: voidCharge,
  async purge(id: string) {
    return fetch(`/v1/admin/purge/${id}`, { method: "DELETE" });
  },
};

const settings = {
  async read() {
    return fetch("/v1/admin/settings");
  },
};

function voidCharge(id: string) {
  return fetch(`/v1/charges/${id}/void`, { method: "POST" });
}
