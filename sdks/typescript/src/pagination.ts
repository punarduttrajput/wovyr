import type { Page, PageParams } from "./types.js";

/** Auto-iterates a cursor-paginated list endpoint, yielding one item at a
 * time and fetching the next page on demand. Works with any resource method
 * shaped like `(params?: PageParams) => Promise<Page<T>>` — e.g.
 * `paginateAll(params => client.agents.list(params))`. Stops once
 * `has_more` is `false` or `next_cursor` is `null`, whichever comes first. */
export async function* paginateAll<T, P extends PageParams = PageParams>(
  fetchPage: (params: P) => Promise<Page<T>>,
  params?: Omit<P, "cursor">,
): AsyncGenerator<T> {
  let cursor: string | undefined;
  for (;;) {
    const page = await fetchPage({ ...(params as P), cursor });
    for (const item of page.data) {
      yield item;
    }
    if (!page.has_more || !page.next_cursor) return;
    cursor = page.next_cursor;
  }
}
