import { record } from "@fixture/storage-kit/beacon";
import { BlobStore } from "@fixture/storage-kit/blob-store";
import { drain } from "@fixture/storage-kit/queue-store";

export const archive = async (key: string, body: string): Promise<void> => {
  const store = new BlobStore("eu-west-1");

  await store.put(key, body);
  await drain();
  record("archived");
};
