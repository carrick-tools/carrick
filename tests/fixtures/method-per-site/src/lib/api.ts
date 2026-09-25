import http from "@example/http";

export const api = http.create({ baseURL: process.env.API_URL });

export async function currentUser(): Promise<unknown> {
  const response = await api.get("/me");
  return response.data;
}
