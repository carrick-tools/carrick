import { apiFetch } from "./http.js";
import type { Order } from "./types.js";

export const ordersApi = {
  list: async (): Promise<{ orders: Order[] }> => {
    const response = await apiFetch("/v1/orders?mine=true");
    return response.json();
  },

  downloadFile: async (orderId: string, fileId: string): Promise<Blob> => {
    const response = await apiFetch(`/v1/orders/${orderId}/files/${fileId}`);
    if (!response.ok) {
      throw new Error(`Failed to download file: ${response.status}`);
    }
    return response.blob();
  },

  addNote: async (orderId: string, body: string): Promise<Order> => {
    const formData = new FormData();
    formData.append("body", body);
    const response = await apiFetch(`/v1/orders/${orderId}/notes`, {
      method: "POST",
      body: formData,
    });
    if (!response.ok) {
      throw new Error(`Failed to post note: ${response.status}`);
    }
    return response.json() as Promise<Order>;
  },

  async setStatus(orderId: string, status: string): Promise<Order> {
    const response = await apiFetch(`/v1/orders/${orderId}/status`, {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ status }),
    });
    return response.json() as Promise<Order>;
  },
};
