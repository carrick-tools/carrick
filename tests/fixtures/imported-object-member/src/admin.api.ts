import * as api from "./http.js";

export const ordersApi = {
  addNote: async (orderId: string, body: string): Promise<Response> => {
    const response = await api.apiFetch(`/admin/orders/${orderId}/notes`, { method: "POST", body });
    return response;
  },
};
