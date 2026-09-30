import { client } from "./client.js";

export const uploads = {
  send(file: Blob) {
    return client.request({ method: "POST", url: "/v1/uploads", data: file });
  },
};
