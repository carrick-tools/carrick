import { apiOrigin } from "./config";

export interface Shelf {
  id: string;
  name: string;
}

export const fetchShelf = async (shelfId: string, init?: RequestInit) => {
  const response = await fetch(`${apiOrigin()}/v1/shelves/${encodeURIComponent(shelfId)}`, init);
  return (await response.json()) as Shelf;
};
