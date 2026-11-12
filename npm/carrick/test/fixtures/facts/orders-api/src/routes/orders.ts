import { Router } from "express";
import type { Request, Response } from "express";
import { db } from "../db";

export const router = Router();

// Reads one order by id.
// The handler below is registered on the router further down.

/* A block comment with a brace { that is not code. */
export async function getOrder(req: Request, res: Response): Promise<void> {
  const order = await db.orders.find(req.params["id"]);
  if (!order) {
    res.status(404).json({ error: "not found" });
    return;
  }
  res.json(order);
}

router.get("/:id", getOrder);

// Creates an order; this handler is written inline, so it has no name.
router.post("/", async (req: Request, res: Response) => {
  const created = await db.orders.create(req.body);
  res.status(201).json(created);
});
