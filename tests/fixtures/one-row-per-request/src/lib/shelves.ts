const SHELF_API_URL = process.env.SHELF_API_URL ?? "";

export interface ShelfSummary {
  shelves: Record<string, boolean>;
  version: string;
}

async function sendWithAuth(url: string, init?: RequestInit): Promise<Response> {
  return fetch(url, {
    ...init,
    headers: { authorization: "Bearer test" },
  });
}

export const shelvesApi = {
  async listMine(): Promise<ShelfSummary> {
    const response = await sendWithAuth(`${SHELF_API_URL}/v1/me/shelves`);
    if (!response.ok) {
      throw new Error("shelf lookup failed");
    }
    return response.json();
  },
};
