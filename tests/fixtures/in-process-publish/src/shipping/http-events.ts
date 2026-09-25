export class HttpEvents {
  private readonly sent: string[] = [];

  publish(topic: string, body: string): Promise<Response> {
    this.sent.push(topic);
    return fetch(`/events/${topic}`, { method: "POST", body });
  }
}
