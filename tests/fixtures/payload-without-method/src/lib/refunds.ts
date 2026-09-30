import { client } from "./client.js";
import { logger } from "./logger.js";

export const refunds = {
  async issue(body: { orderId: string; amount: number }) {
    logger.info({ body, event: "refund.issue" });
    return client.createRefund(body);
  },
};
