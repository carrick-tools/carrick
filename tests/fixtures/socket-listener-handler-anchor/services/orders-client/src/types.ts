export interface OrderUpdate {
  id: string;
  status: "open" | "shipped";
}
export interface OrderEvents {
  "order:update": (message: OrderUpdate) => void;
  "order:ping": (message: OrderUpdate) => void;
  "order:any": (message: any) => void;
}
