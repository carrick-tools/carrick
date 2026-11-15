// This file is generated from the API description. Do not edit it.

export type RefundBody = { id: string; amount: number; reason: string };

/** GET /api/v1/refunds/{refundId} */
export declare function getRefund(options: {
  path: { refundId: string };
}): Promise<{ data: RefundBody }>;
