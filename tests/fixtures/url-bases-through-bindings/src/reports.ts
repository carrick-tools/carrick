import { serverConfig } from "./config.ts";

export async function listReports() {
  const res = await fetch(`${serverConfig.reportsUrl}/v1/reports`, {
    method: "GET",
  });
  return res.json();
}
