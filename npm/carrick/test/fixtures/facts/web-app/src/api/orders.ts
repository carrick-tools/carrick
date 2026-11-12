import { api } from "./client";

export type Order = { id: string; total: number };

export const loadOrder = async (id: string): Promise<Order> => {
  const response = await api.get(`/orders/${id}`);
  return response.data as Order;
};
