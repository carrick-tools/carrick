import express from "express";

// End-to-end harness only: stands up a stub on the SAME path the package
// really serves, so the suite can assert what was posted without a network.
// This is not a route this package serves, and must never become a producer
// row (carrick#588 defect 1).
export function startStubCollector() {
  const app = express();

  app.post("/v1/reports", (req, res) => {
    res.status(201).json({ id: "stub" });
  });

  return app.listen(0);
}
