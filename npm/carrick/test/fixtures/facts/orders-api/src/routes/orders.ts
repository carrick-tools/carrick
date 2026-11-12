import { Router } from "express";
import type { Request, Response } from "express";
import { db } from "../db";

export const router = Router();

// Reads one order by id.
// The handler below is registered on the router at the end of this file.

/* A block comment with a brace { that is not code. */
export async function getOrder(req: Request, res: Response): Promise<void> {
  const order = await db.orders.find(req.params["id"]);
  if (!order) {
    res.status(404).json({ error: "not found" });
    return;
  }
  res.json(order);
}

export async function cancelOrder(req: Request, res: Response): Promise<void> {
  await db.orders.cancel(req.params["id"]);
  res.status(204).end();
}

router.get("/orders/:id", getOrder);
router.post("/orders/:id/cancel", cancelOrder);
