import type { ordersApi } from "./orders.api.js";

export const replyVia = (api: typeof ordersApi, orderId: string) => api.addNote(orderId, "ok");
