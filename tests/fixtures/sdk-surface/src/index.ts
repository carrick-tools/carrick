import * as API from "./resources/index.js";
import { auditLog } from "./util/audit.js";
import { chargeCard } from "./util/direct.js";

export default class Ledger {
  payments: API.Payments = new API.Payments(this);
  reports: API.Reports = new API.Reports(this);

  constructor(private readonly baseUrl: string) {}

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
