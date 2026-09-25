export class SocketPublisher {
  private readonly socket = new WebSocket("wss://relay.internal/events");

  publish(topic: string, body: string): void {
    this.socket.send(`${topic}:${body}`);
  }
}
