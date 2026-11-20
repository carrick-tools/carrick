import type { NextFunction, Request, RequestHandler, Response } from 'express';

type Handler = (req: Request, res: Response) => Promise<void> | void;

// Hands an async handler's rejection to the error middleware.
export function wrap(handler: Handler): RequestHandler {
  return (req: Request, res: Response, next: NextFunction) => {
    Promise.resolve(handler(req, res)).catch(next);
  };
}
