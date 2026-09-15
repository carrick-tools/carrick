// The payment vendor's API origin, shared by every module that talks to it.
export const PAYMENTS_VENDOR_BASE = "https://api.payments-vendor.example";

export interface Charge {
  id: string;
  amount: number;
}
