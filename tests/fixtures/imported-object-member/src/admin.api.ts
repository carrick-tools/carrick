import { apiFetch } from "./http.js";

export const ordersApi = {
  addNote: async (orderId: string, body: string): Promise<Response> => {
    const response = await apiFetch(`/admin/orders/${orderId}/notes`, { method: "POST", body });
    return response;
  },
};
