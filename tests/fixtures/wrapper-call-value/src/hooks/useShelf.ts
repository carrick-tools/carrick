import { fetchShelf } from "../lib/shelf";

export const useShelf = async (shelfId: string, init?: RequestInit) => {
  const shelf = await fetchShelf(shelfId, init);
  return shelf;
};
