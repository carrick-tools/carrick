import { fetchShelfStats } from "../lib/shelves";

export const useShelfStats = (shelfId?: string) => {
  const request = fetchShelfStats(shelfId);
  return request;
};
