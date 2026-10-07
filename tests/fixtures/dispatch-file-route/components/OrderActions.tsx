'use client';

export function OrderActions({ orderId }: { orderId: string }) {
  async function confirm() {
    await fetch(`/api/orders/${orderId}`, {
      method: 'POST',
      body: JSON.stringify({ op: 'confirm' }),
    });
  }

  async function cancel(reason: string) {
    await fetch(`/api/orders/${orderId}`, {
      method: 'POST',
      body: JSON.stringify({ op: 'cancel', reason }),
    });
  }

  async function refund(amount: number) {
    await fetch(`/api/orders/${orderId}`, {
      method: 'POST',
      body: JSON.stringify({ op: 'refund', amount }),
    });
  }

  async function load() {
    const response = await fetch(`/api/orders/${orderId}`);
    return response.json();
  }

  return (
    <div onLoad={load}>
      <button onClick={confirm}>Confirm</button>
      <button onClick={() => cancel('customer request')}>Cancel</button>
      <button onClick={() => refund(10)}>Refund</button>
    </div>
  );
}
