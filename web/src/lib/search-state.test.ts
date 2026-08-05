import { describe, expect, it } from "vitest";

import {
  DEFAULT_STATE,
  paramsToState,
  stateToParams,
  threadFromParams,
  threadToParams,
  type ThreadState,
} from "@/lib/search-state";

describe("threadToParams / threadFromParams", () => {
  it("round-trips a thread without a focused comment", () => {
    const thread: ThreadState = { rootId: 44123 };
    const params = threadToParams(thread, new URLSearchParams());

    expect(params.get("item")).toBe("44123");
    expect(params.get("c")).toBeNull();
    expect(threadFromParams(params)).toEqual({ rootId: 44123 });
  });

  it("round-trips a thread with a focused comment", () => {
    const thread: ThreadState = { rootId: 44123, focusId: 44567 };
    const params = threadToParams(thread, new URLSearchParams());

    expect(threadFromParams(params)).toEqual({ rootId: 44123, focusId: 44567 });
  });

  it("writes nothing when there is no thread open", () => {
    const params = threadToParams(null, new URLSearchParams());

    expect(params.toString()).toBe("");
  });

  it("reads no thread from params that have none", () => {
    expect(threadFromParams(new URLSearchParams("q=rust"))).toBeNull();
  });

  it("rejects a non-numeric or non-positive item id", () => {
    expect(threadFromParams(new URLSearchParams("item=abc"))).toBeNull();
    expect(threadFromParams(new URLSearchParams("item=0"))).toBeNull();
    expect(threadFromParams(new URLSearchParams("item=-5"))).toBeNull();
  });

  it("ignores a malformed focus id but keeps the thread", () => {
    expect(threadFromParams(new URLSearchParams("item=44123&c=abc"))).toEqual({
      rootId: 44123,
    });
  });

  it("coexists with search params without disturbing them", () => {
    const params = stateToParams({ ...DEFAULT_STATE, q: "rust", scope: "comments" });
    threadToParams({ rootId: 44123 }, params);

    expect(paramsToState(params).q).toBe("rust");
    expect(paramsToState(params).scope).toBe("comments");
    expect(threadFromParams(params)?.rootId).toBe(44123);
  });
});
