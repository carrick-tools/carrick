import { request } from "./transport";

export interface ShelfStats {
  shelfId: string;
  books: number;
}

const parseShelfStats = (payload: unknown): ShelfStats => {
  const body = payload as Record<string, unknown>;
  return { shelfId: String(body.shelfId), books: Number(body.books) };
};

export const fetchShelfStats = (shelfId?: string) => {
  const path = shelfId
    ? `/v1/shelves/stats?shelfId=${encodeURIComponent(shelfId)}`
    : "/v1/shelves/stats";

  return request({ path, method: "GET" }).mapOk(parseShelfStats);
};
