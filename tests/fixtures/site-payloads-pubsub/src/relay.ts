const relay = {
  send(topic: string, payload: unknown): void {
    void topic;
    void payload;
  },
};

export function forward(sku: string): void {
  relay.send('orders.shipped', { topic: 'orders.shipped', sku });
}
