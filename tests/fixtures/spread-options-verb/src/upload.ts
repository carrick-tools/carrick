export type UploadOptions = { url: string; request?: RequestInit };

export function sendUpload(options: UploadOptions): Promise<Response> {
  return fetch(options.url, { method: "POST", ...options.request });
}
