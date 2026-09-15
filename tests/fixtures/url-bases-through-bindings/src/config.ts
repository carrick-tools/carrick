// Every base URL the client talks to is read once, here, from the environment,
// with a local default beside it. The browser bundle reads its build-time env;
// the server-side half reads the process env through the runtime's own API.
export default {
  ORDERS_API_URL: import.meta.env.VITE_ORDERS_API_URL || "http://localhost:4000",
  SHOW_BANNER: import.meta.env.VITE_SHOW_BANNER !== "false",
};

export const serverConfig = {
  reportsUrl: Deno.env.get("REPORTS_URL") ?? "http://localhost:4200",
};
