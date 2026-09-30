import { uploads } from "./lib/uploads.js";

export function uploadFile(file: Blob) {
  return uploads.send(file);
}
