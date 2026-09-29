/** Starts polling the moment it is built: the constructor sends the request. */
export class Poller {
  constructor(url: string) {
    void fetch(url, { method: "GET" });
  }
}
