import { invoke } from "@tauri-apps/api/core";

export function useNodeInit(onReady: () => Promise<void>) {
  let nodeId = $state("");
  let pubkey = $state("");
  let loading = $state(true);

  async function init() {
    try {
      [nodeId, pubkey] = await Promise.all([
        invoke<string>("get_node_id"),
        invoke<string>("get_pubkey"),
      ]);
      await onReady();
      loading = false;
    } catch {
      setTimeout(init, 500);
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
    init,
  };
}
