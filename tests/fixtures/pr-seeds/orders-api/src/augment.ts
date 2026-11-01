import "./contracts/order-ref";

declare module "./contracts/order-ref" {
  interface OrderMeta {
    channel?: string;
  }
}
