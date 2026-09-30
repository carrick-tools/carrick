import { readStatus } from "./status.js";

export function loadStatus(apiUrl: string, id: string) {
  return readStatus(`${apiUrl}/api/v1/status/${id}`);
}
