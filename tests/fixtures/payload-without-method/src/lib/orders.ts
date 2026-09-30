import { client } from "./client.js";
import { decodeEnvelope } from "./envelope.js";

export const orders = {
  place(body: { sku: string }) {
    return client.createOrder(body);
  },
  async decode(output: unknown) {
    return decodeEnvelope({ data: output, format: "json" }, client);
  },
};
