import { callApi, Order } from "./seed-generic-lib";
export const orders = () => callApi<Order[]>("/orders");
