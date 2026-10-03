import { ordersApi } from "./admin.api.js";

export const flagOrder = (orderId: string) => ordersApi.addNote(orderId, "flagged");
