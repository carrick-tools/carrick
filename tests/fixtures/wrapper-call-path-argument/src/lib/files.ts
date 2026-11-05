import { request } from "./transport";

export const fetchShelfFile = (name: string) => {
  return request({
    path: `/v1/shelves/files/${name}`,
    method: "GET",
  });
};
