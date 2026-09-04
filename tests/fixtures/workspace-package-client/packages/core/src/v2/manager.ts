import { ApiClient } from "./client/index.js";

// The factory a consumer asks for a client. It is the imported binding the
// receiver's origin traces back to; the client itself is never imported.
export const clientManager = {
  clientOrThrow(): ApiClient {
    return new ApiClient(process.env.CORE_API_URL ?? "http://localhost:3000");
  },
};
