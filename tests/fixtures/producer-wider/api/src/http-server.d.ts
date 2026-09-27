declare module "http-server" {
  export interface Server {
    get(path: string, handler: (request: unknown) => unknown): void;
  }
  export function createServer(): Server;
}
