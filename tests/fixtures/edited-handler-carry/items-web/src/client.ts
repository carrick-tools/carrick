import { send } from "./send.js";

export type Item = {
  id: string;
  connected: boolean;
};

export type ItemStatus = {
  id: string;
  state: string;
};

export class ItemsClient {
  constructor(private readonly baseUrl: string) {}

  readItem(id: string): Promise<Item> {
    return send<Item>(`${this.baseUrl}/api/v1/items/${id}`, {
      method: "GET",
    });
  }

  readStatus(id: string): Promise<ItemStatus> {
    return send<ItemStatus>(`${this.baseUrl}/api/v1/items/${id}/status`, {
      method: "GET",
    });
  }

  listItems(): Promise<Item[]> {
    return send<Item[]>(`${this.baseUrl}/api/v1/items`, {
      method: "GET",
    });
  }
}
