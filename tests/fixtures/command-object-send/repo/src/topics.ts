import { SNSClient, PublishCommand } from "@aws-sdk/client-sns";
import type { OrderPlaced } from "./types";

const client = new SNSClient({ region: "eu-west-1" });

export async function announce(evt: OrderPlaced): Promise<void> {
  await client.send(
    new PublishCommand({
      TopicArn: "arn:example:order-events",
      Message: JSON.stringify(evt),
    }),
  );
}
