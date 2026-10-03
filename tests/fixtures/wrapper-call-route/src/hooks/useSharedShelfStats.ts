import { fetchSharedShelfStats } from "../lib/shelves";

export const useSharedShelfStats = (shelfId: string) => {
  const request = fetchSharedShelfStats(shelfId);
  return request;
};
