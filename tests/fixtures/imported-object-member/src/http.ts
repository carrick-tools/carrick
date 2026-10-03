import { tracer } from "trace-kit";

const API_URL = process.env.API_URL;

// The request sits in a callback handed to a package's span helper, so no
// pass reads a request through this helper.
export const apiFetch = (path: string, init?: RequestInit): Promise<Response> =>
  tracer.span(`REST ${init?.method ?? "GET"} ${path}`, async () => {
    const response = await fetch(`${API_URL}${path}`, { ...init, credentials: "include" });
    return response;
  });
