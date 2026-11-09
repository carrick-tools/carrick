import { transport, type RequestOptions } from "./transport";

export { transport } from "./transport";
export type { RequestOptions, RequestSpec, Transport } from "./transport";

/** A request written as a spec: the verb is the member, the path is `url`. */
export const refreshFeed = (options: RequestOptions) =>
  (options.client ?? transport).post({ url: "/api/v1/boards/{board}/feeds/{feed}/refresh", ...options });

export const archiveFeed = (options: RequestOptions) =>
  (options.client ?? transport).post({ url: "/api/v1/boards/{board}/feeds/{feed}/archive", ...options });
