import { http } from "./transports.js";
import type { Transport } from "./ports.js";

export class HttpTransport implements Transport {
  async send(body: string) {
    return http.post("/api/events", body);
  }
}
