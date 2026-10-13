import { SENTINEL_MARGIN_PX, setupInfiniteScroll } from "$lib/utils";

/**
 * `loadMoreFn` loads the next page. It returns `false` when the load failed;
 * any other result counts as completed. After a completed load, the next page
 * is requested if the sentinel is still within `SENTINEL_MARGIN_PX` of the
 * viewport (the IntersectionObserver reports only changes, so it does not
 * fire again while the sentinel stays visible).
 */
export function useInfiniteScroll(
  getSentinel: () => HTMLElement | null,
  loadMoreFn: () => Promise<boolean | void>,
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

  /** Mark the list as having more pages. */
  function reset() {
    hasMore = true;
  }

  function sentinelReached(): boolean {
    const sentinel = getSentinel();
    if (!sentinel || !sentinel.isConnected) return false;
    const rect = sentinel.getBoundingClientRect();
    return (
      rect.bottom >= 0 && rect.top <= window.innerHeight + SENTINEL_MARGIN_PX
    );
  }

  async function loadMore() {
    if (loadingMore || !hasMore) return;
    loadingMore = true;
    let completed = false;
    try {
      completed = (await loadMoreFn()) !== false;
    } finally {
      loadingMore = false;
    }
    if (completed) checkSentinel();
  }

  /**
   * After the next frame, request the next page if the sentinel is within
   * range. For layout changes that the IntersectionObserver does not report,
   * such as appended rows or a filter that leaves the sentinel where it was.
   */
  function checkSentinel() {
    requestAnimationFrame(() => {
      if (hasMore && !loadingMore && sentinelReached()) loadMore();
    });
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
    reset,
    loadMore,
    checkSentinel,
    setupEffect,
  };
}
