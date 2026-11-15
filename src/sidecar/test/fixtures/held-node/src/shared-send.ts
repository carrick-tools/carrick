import type { Response } from "express-shape";

export function sendShared(res: Response, label: string): void {
  res.json({ label });
}
