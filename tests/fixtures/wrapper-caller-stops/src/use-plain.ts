import { getItem } from "./clients/plain";

export async function loadOne(): Promise<Response> {
  return getItem("a1");
}
