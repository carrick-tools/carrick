import { readFile } from "node:fs/promises";
import { createHash } from "crypto";
import { shared } from "@fixture/internal-lib";
import { helper } from "./helper";

export async function load(path: string): Promise<string> {
  const raw = await readFile(path, "utf8");
  createHash("sha256");
  return helper(shared(raw));
}
