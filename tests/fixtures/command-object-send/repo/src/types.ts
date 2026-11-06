export interface OrderPlaced {
  orderId: string;
  total: number;
}

export interface Refund {
  orderId: string;
  amount: number;
}

declare const process: { env: Record<string, string | undefined> };
export const env = process.env;
