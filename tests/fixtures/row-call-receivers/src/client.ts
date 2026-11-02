// Local stub transports: no packages, one per protocol.
const http = { get: (url: string): Promise<string> => Promise.resolve(url) };
const gql = { request: (query: string): Promise<string> => Promise.resolve(query) };
const socket = { emit: (event: string, payload: string): void => void [event, payload] };
const queue = { publish: (topic: string, body: string): void => void [topic, body] };
const rpc = { call: (name: string): Promise<string> => Promise.resolve(name) };

export class Client {
  getUser(): Promise<string> {
    return http.get("/users/1");
  }

  runQuery(): Promise<string> {
    return gql.request("{ users { id } }");
  }

  notify(): void {
    socket.emit("user:updated", "1");
  }

  enqueue(): void {
    queue.publish("user-events", "1");
  }

  callProcedure(): Promise<string> {
    return rpc.call("users.list");
  }
}
