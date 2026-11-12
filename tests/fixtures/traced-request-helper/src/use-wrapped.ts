import { callApi } from "./wrapped";

export async function addItem(item: { name: string }): Promise<Response> {
  return await callApi("POST", "/v1/items", item);
}

export async function listItems(): Promise<Response> {
  return await callApi("GET", "/v1/items");
}
