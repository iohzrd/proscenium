import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export function useEventListeners(
  listeners: Record<string, (payload: unknown) => void>,
) {
  const unlisteners: Promise<UnlistenFn>[] = [];
  for (const [event, handler] of Object.entries(listeners)) {
    unlisteners.push(listen(event, (e) => handler(e.payload)));
  }
  return () => {
    unlisteners.forEach((p) => p.then((fn) => fn()));
  };
}
