import { createServer } from "http-server";
import { router } from "./router";

const app = createServer();
app.use("/api", router);
