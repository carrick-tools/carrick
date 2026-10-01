import { job, queues } from "@fixture/jobs";

// The design's own counterexample (D2): the optional `queue` is the routing
// name, and `id` only labels the job. A claim that `id` is the name is wrong,
// and only a sibling rule that counts optional keys can refuse it.
export const resizeImage = job({
  id: "resize-image",
  queue: "images",
  run: async (payload: { url: string }) => {
    console.log(payload.url);
  },
});

export async function upload(url: string) {
  await queues.enqueue("images", { url });
}
