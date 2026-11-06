import { beforeEach, describe, expect, it } from "vitest";

import type { HNHit } from "@/lib/meili";
import { clearStoryCache, resolveStories, type BatchFetch } from "@/lib/story-context";

type Item = Pick<HNHit, "id" | "type" | "parent" | "title">;

const story = (id: number, title = `story ${id}`): Item => ({ id, type: "story", title });
const reply = (id: number, parent: number): Item => ({ id, type: "comment", parent });

/** A fake index over `items` that records every batch it is asked for. */
function fakeFetch(items: Item[]) {
  const byId = new Map(items.map((item) => [item.id, item]));
  const calls: number[][] = [];
  const fetch: BatchFetch = async (ids) => {
    calls.push([...ids].sort((a, b) => a - b));
    return ids.flatMap((id) => (byId.has(id) ? [byId.get(id)!] : []));
  };
  return { fetch, calls };
}

beforeEach(() => clearStoryCache());

describe("resolveStories", () => {
  it("names the story behind direct and nested replies", async () => {
    const { fetch } = fakeFetch([story(1, "Show HN: X"), reply(2, 1), reply(3, 2), reply(4, 3)]);

    const stories = await resolveStories(
      [
        { id: 2, parent: 1 },
        { id: 4, parent: 3 },
      ],
      fetch,
    );

    expect(stories.get(2)).toEqual({ id: 1, title: "Show HN: X" });
    expect(stories.get(4)).toEqual({ id: 1, title: "Show HN: X" });
  });

  it("batches every hit's ancestor into one lookup per level", async () => {
    const { fetch, calls } = fakeFetch([
      story(1),
      story(10),
      reply(2, 1),
      reply(11, 10),
      reply(12, 11),
    ]);

    await resolveStories(
      [
        { id: 3, parent: 2 },
        { id: 13, parent: 12 },
      ],
      fetch,
    );

    // Level by level: [2, 12] → [1, 11] → [10]. Never one call per hit.
    expect(calls).toEqual([[2, 12], [1, 11], [10]]);
  });

  it("reports shallow replies before the deepest chain finishes", async () => {
    const { fetch } = fakeFetch([story(1), story(10), reply(11, 10), reply(12, 11)]);
    const progress: number[][] = [];

    const stories = await resolveStories(
      [
        { id: 2, parent: 1 },
        { id: 13, parent: 12 },
      ],
      fetch,
      (partial) => progress.push([...partial.keys()]),
    );

    expect(progress[0]).toEqual([2]);
    expect([...stories.keys()].sort()).toEqual([13, 2]);
  });

  it("reuses ancestors read on an earlier page", async () => {
    const { fetch, calls } = fakeFetch([story(1), reply(2, 1)]);

    await resolveStories([{ id: 3, parent: 2 }], fetch);
    const before = calls.length;
    const stories = await resolveStories([{ id: 4, parent: 2 }], fetch);

    expect(calls.length).toBe(before);
    expect(stories.get(4)?.id).toBe(1);
  });

  it("leaves out hits whose chain is broken", async () => {
    // 2's parent 1 was deleted, so it was never indexed.
    const { fetch } = fakeFetch([reply(2, 1)]);

    const stories = await resolveStories([{ id: 3, parent: 2 }], fetch);

    expect(stories.size).toBe(0);
  });

  it("terminates on a parent cycle", async () => {
    const { fetch } = fakeFetch([reply(2, 3), reply(3, 2)]);

    const stories = await resolveStories([{ id: 4, parent: 2 }], fetch);

    expect(stories.size).toBe(0);
  });
});
