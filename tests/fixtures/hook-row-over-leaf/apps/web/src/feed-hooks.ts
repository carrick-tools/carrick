import { useMutation } from "@fixture/query";
import { startArchive, startRefresh, type RefreshBody } from "./feed-api";
declare const cache: { invalidate(key: string): void };

/** The callback calls the app's own function; the model names the path's holes its own way. */
export function useRefreshFeed(boardId: string, feedId: string) {
  return useMutation({
    mutationFn: (variables: RefreshBody | void) => {
      return startRefresh(boardId, feedId, variables ?? undefined);
    },
    onSuccess: () => cache.invalidate("feeds"),
  });
}

/** The same shape, where the model names the holes as the client does. */
export function useArchiveFeed(board: string, feed: string) {
  return useMutation({
    mutationFn: () => {
      return startArchive(board, feed);
    },
    onSuccess: () => cache.invalidate("feeds"),
  });
}
