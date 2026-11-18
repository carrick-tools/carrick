import { http } from "@fixture/http";
export interface Order { id: string }
export function listOrders(p: string): Promise<Order[]> {
  return http.post<any>(`${p}/orders-catch`).catch(() => []);
}
