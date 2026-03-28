import { setupInfiniteScroll } from "$lib/utils";

export function useInfiniteScroll(
  getSentinel: () => HTMLElement | null,
  loadMoreFn: () => Promise<void>,
  pageSize: number,
) {
  let hasMore = $state(true);
  let loadingMore = $state(false);

  function setHasMore(count: number) {
    hasMore = count >= pageSize;
  }

  function setNoMore() {
    hasMore = false;
  }

  async function loadMore() {
    if (loadingMore || !hasMore) return;
    loadingMore = true;
    await loadMoreFn();
    loadingMore = false;
  }

  function setupEffect() {
    return setupInfiniteScroll(
      getSentinel(),
      () => hasMore,
      () => loadingMore,
      loadMore,
    );
  }

  return {
    get hasMore() {
      return hasMore;
    },
    get loadingMore() {
      return loadingMore;
    },
    setHasMore,
    setNoMore,
    loadMore,
    setupEffect,
  };
}
