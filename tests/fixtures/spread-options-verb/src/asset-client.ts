import { sendUpload } from "./upload.js";

export function replaceAsset(apiUrl: string, id: string, file: Blob) {
  return sendUpload({
    url: `${apiUrl}/api/v1/assets/${id}`,
    request: { method: "PUT", body: file },
  });
}
