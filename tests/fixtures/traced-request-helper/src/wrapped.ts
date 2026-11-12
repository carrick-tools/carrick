import { send } from "./traced";

// A module function that takes the method and the body as arguments and
// hands them to the traced helper.
export function callApi(method: string, endpoint: string, data?: unknown): Promise<Response> {
  return send(endpoint, { method, body: JSON.stringify(data) });
}
