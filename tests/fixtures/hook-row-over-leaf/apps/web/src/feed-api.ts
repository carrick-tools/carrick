import { archiveFeed, refreshFeed } from "@fixture/client";

export interface RefreshBody {
  windowMinutes?: number;
}

/** The app's own function: it calls the client, which writes the request. */
export function startRefresh(board: string, feed: string, body?: RefreshBody) {
  return refreshFeed({ path: { board, feed }, body });
}

export function startArchive(board: string, feed: string) {
  return archiveFeed({ path: { board, feed } });
}
