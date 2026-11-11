import { connect } from "fixture-broker";

const broker = connect("amqp://broker.internal");

export function publishOrder(id: string): Promise<void> {
  return broker.publish("orders.created", { id });
}

export function publishRegional(id: string): Promise<void> {
  return broker.publish(`orders.${process.env.REGION}`, { id });
}
