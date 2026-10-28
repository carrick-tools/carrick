import app from "fixture-chain-router";
import { items } from "./items";

const root = app();
// A router built in another module, mounted under a prefix.
root.use("/api", items);
