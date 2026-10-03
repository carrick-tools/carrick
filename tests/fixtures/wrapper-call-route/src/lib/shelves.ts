import { request } from "./transport";

export interface ShelfStats {
  shelfId: string;
  books: number;
}

export interface SharedShelfStats {
  shelfId: string;
  readers: number;
}

export const fetchShelfStats = (shelfId?: string) => {
  const path = shelfId
    ? `/v1/shelves/stats?shelfId=${encodeURIComponent(shelfId)}`
    : "/v1/shelves/stats";

  return request({ path, method: "GET" });
};

export const fetchSharedShelfStats = (shelfId: string) => {
  return request({
    path: `/v1/shelves/shared-stats?shelfId=${encodeURIComponent(shelfId)}`,
    method: "GET",
  });
};
