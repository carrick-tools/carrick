export class HttpEvents {
  publish(topic: string, body: string): Promise<Response> {
    return fetch(`/events/${topic}`, { method: "POST", body });
  }
}
