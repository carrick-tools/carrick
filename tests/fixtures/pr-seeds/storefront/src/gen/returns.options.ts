// This file is generated from the API description. Do not edit it.
import type { QueryOptions } from "../query-kit.js";

export type ReturnBody = { id: string; items: number; status: string };

export const getReturnOptions = (returnId: string): QueryOptions<ReturnBody> => ({
  queryKey: ["return", returnId],
  queryFn: async () => {
    const response = await fetch(`/api/v1/returns/${returnId}`);
    return (await response.json()) as ReturnBody;
  },
});
