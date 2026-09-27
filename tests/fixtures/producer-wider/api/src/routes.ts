import { createServer } from "http-server";
import { listHolidays } from "./holidays";

const app = createServer();

app.get("/holidays", () => listHolidays());
