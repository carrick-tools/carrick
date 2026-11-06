import { SQSClient, SendMessageCommand } from "@aws-sdk/client-sqs";
import { env, type OrderPlaced, type Refund } from "./types";

const client = new SQSClient({ region: "eu-west-1" });

export async function queueOrderPlaced(evt: OrderPlaced): Promise<void> {
  await client.send(
    new SendMessageCommand({
      QueueUrl: "https://sqs.example.test/000000000000/order-placed",
      MessageBody: JSON.stringify(evt),
    }),
  );
}

export async function queueAudit(evt: OrderPlaced): Promise<void> {
  await client.send(
    new SendMessageCommand({
      QueueUrl: env.AUDIT_QUEUE_URL,
      MessageBody: JSON.stringify(evt),
    }),
  );
}

async function sendTo(url: string, body: string): Promise<void> {
  await client.send(new SendMessageCommand({ QueueUrl: url, MessageBody: body }));
}

export async function queueRefund(refund: Refund): Promise<void> {
  await sendTo(env.REFUND_QUEUE_URL ?? "", JSON.stringify(refund));
}

export async function queueShipment(evt: OrderPlaced): Promise<void> {
  await sendTo(env.SHIPMENT_QUEUE_URL ?? "", JSON.stringify(evt));
}
