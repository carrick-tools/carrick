import { api } from "../api/client";

export async function checkPage001(): Promise<boolean> {
  const response = await api.get("/health");
  return response.status === 200;
}
