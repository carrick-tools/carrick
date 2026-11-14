import type { RouterRequest, RouterResponse } from "fixture-router";

export const stats = (req: RouterRequest, res: RouterResponse): void => {
  if (req.path === "/stats") res.send(1);
};
