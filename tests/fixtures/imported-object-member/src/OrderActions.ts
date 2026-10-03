import { ordersApi } from "./orders.api.js";

export function orderActions(orderId: string, setError: (message: string | null) => void) {
  const download = (fileId: string) => ordersApi.downloadFile(orderId, fileId);

  const reply = (note: string) =>
    ordersApi
      .addNote(orderId, note.trim())
      .then(() => setError(null))
      .catch(() => setError("Reply failed"));

  const approve = () => ordersApi.setStatus(orderId, "approved");

  return { download, reply, approve };
}
