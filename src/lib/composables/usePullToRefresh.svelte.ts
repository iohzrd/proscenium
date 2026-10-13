export function usePullToRefresh(onRefresh: () => Promise<void>) {
  let pullStartY = 0;
  let pullDistance = $state(0);
  let isPulling = $state(false);
  let pullTriggered = $state(false);
  // True while `onRefresh` is pending; touch events are ignored during that time.
  let refreshing = false;
  const PULL_THRESHOLD = 80;

  function handleTouchStart(e: TouchEvent) {
    if (refreshing) return;
    if (window.scrollY === 0) {
      pullStartY = e.touches[0].clientY;
      isPulling = true;
    }
  }

  function handleTouchMove(e: TouchEvent) {
    if (!isPulling || refreshing) return;
    const delta = e.touches[0].clientY - pullStartY;
    if (delta > 0) {
      pullDistance = Math.min(delta * 0.5, 120);
      pullTriggered = pullDistance >= PULL_THRESHOLD;
    } else {
      pullDistance = 0;
      isPulling = false;
    }
  }

  async function handleTouchEnd() {
    if (refreshing) return;
    if (isPulling && pullTriggered) {
      refreshing = true;
      try {
        await onRefresh();
      } finally {
        refreshing = false;
      }
    }
    pullDistance = 0;
    isPulling = false;
    pullTriggered = false;
  }

  return {
    get pullDistance() {
      return pullDistance;
    },
    get isPulling() {
      return isPulling;
    },
    get pullTriggered() {
      return pullTriggered;
    },
    handleTouchStart,
    handleTouchMove,
    handleTouchEnd,
  };
}
