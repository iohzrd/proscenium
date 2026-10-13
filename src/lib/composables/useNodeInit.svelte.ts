import { invoke } from "@tauri-apps/api/core";
import { onDestroy } from "svelte";

// Delay before attempt n+1 of the readiness calls; the last value repeats.
const RETRY_DELAYS_MS = [250, 500, 1000, 2000, 4000];
const MAX_ATTEMPTS = 20;

/**
 * Loads the local Node ID and pubkey, then runs `onReady` once.
 * Only the two identity calls are retried (at most `MAX_ATTEMPTS` times with
 * the delays above). A rejection of `onReady`, or exhausted retries, sets
 * `error`; `init()` can be called again. Retries stop when the component is
 * destroyed. Must be called during component initialization.
 */
export function useNodeInit(onReady: () => Promise<void>) {
  let nodeId = $state("");
  let pubkey = $state("");
  let loading = $state(true);
  let error = $state("");
  let running = false;
  let destroyed = false;
  let cancelWait: (() => void) | null = null;

  function wait(ms: number): Promise<void> {
    return new Promise((resolve) => {
      const timer = setTimeout(() => {
        cancelWait = null;
        resolve();
      }, ms);
      cancelWait = () => {
        clearTimeout(timer);
        cancelWait = null;
        resolve();
      };
    });
  }

  onDestroy(() => {
    destroyed = true;
    cancelWait?.();
  });

  async function loadIdentity(): Promise<boolean> {
    for (let attempt = 1; ; attempt++) {
      try {
        const [node, key] = await Promise.all([
          invoke<string>("get_node_id"),
          invoke<string>("get_pubkey"),
        ]);
        if (destroyed) return false;
        nodeId = node;
        pubkey = key;
        return true;
      } catch (e) {
        if (destroyed) return false;
        if (attempt >= MAX_ATTEMPTS) {
          error = `Node is not ready: ${e}`;
          return false;
        }
        const delay =
          RETRY_DELAYS_MS[Math.min(attempt - 1, RETRY_DELAYS_MS.length - 1)];
        await wait(delay);
        if (destroyed) return false;
      }
    }
  }

  async function init() {
    if (running || destroyed) return;
    running = true;
    loading = true;
    error = "";
    try {
      if (!(await loadIdentity())) return;
      try {
        await onReady();
      } catch (e) {
        if (!destroyed) error = String(e);
      }
    } finally {
      running = false;
      if (!destroyed) loading = false;
    }
  }

  return {
    get nodeId() {
      return nodeId;
    },
    get pubkey() {
      return pubkey;
    },
    get loading() {
      return loading;
    },
    /** Empty, or the reason the last `init()` did not complete. */
    get error() {
      return error;
    },
    init,
  };
}
