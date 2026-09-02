import express from "express";

// The real route this package serves.
export const router = express.Router();

router.post("/v1/reports", (req, res) => {
  res.status(201).json({ id: "r-1" });
});
