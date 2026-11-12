import { http } from "./http";

const ORDERS_URL = process.env["ORDERS_URL"];

export const orderResolvers = {
  async order(_parent: unknown, args: { id: string }) {
    return http.get(`${ORDERS_URL}/orders/${args.id}`);
  },
};
