import { sendJson } from "../lib/http";

export function loadStock() {
  return sendJson("GET", "/v2/stock");
}
