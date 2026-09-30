import http from "@fixture/http";
import { config } from "./config";

export const configured = http.create({ baseURL: config.apiUrl });
