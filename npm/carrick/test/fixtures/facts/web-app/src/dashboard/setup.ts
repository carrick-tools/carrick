import { api } from "../api/client";

export function setupDashboard(ids: string[]): void {
  ids.forEach((id) => {
    void api.get(`/orders/${id}`);
  });
}
