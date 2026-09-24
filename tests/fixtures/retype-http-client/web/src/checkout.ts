import { create } from "http-client";

const api = create({ baseURL: "http://localhost:3000" });

export async function typedCheckout(): Promise<number> {
  const response = await api.post<{ x: number }>("/checkout");
  return response.data.x;
}

export async function untypedCheckout(): Promise<number> {
  const response = await api.post("/checkout");
  return response.data.x;
}

export async function fireAndForget(): Promise<void> {
  await api.post("/checkout");
}
