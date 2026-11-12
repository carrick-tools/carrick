export interface Order {
  id: string;
  item: string;
}

export interface OrderBody {
  body: string;
}

export interface Refund {
  id: string;
  amount: number;
}

export interface RefundBody {
  body: number;
}
