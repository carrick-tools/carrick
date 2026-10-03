import { Http } from "@example/http";

const API_URL = process.env.SHELF_API_URL ?? "";

export const request = (options: { path: string; method: string }) => {
  const { path, ...rest } = options;
  return Http.make({ ...rest, url: `${API_URL}${path}` });
};
