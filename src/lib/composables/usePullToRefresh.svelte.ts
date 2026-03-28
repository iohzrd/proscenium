export function usePullToRefresh(onRefresh: () => Promise<void>) {
  let pullStartY = 0;
  let pullDistance = $state(0);
  let isPulling = $state(false);
  let pullTriggered = $state(false);
  const PULL_THRESHOLD = 80;

  function handleTouchStart(e: TouchEvent) {
    if (window.scrollY === 0) {
      pullStartY = e.touches[0].clientY;
      isPulling = true;
    }
  }

  function handleTouchMove(e: TouchEvent) {
    if (!isPulling) return;
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
    if (isPulling && pullTriggered) {
      await onRefresh();
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
