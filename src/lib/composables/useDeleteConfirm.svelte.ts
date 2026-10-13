export function useDeleteConfirm(onDelete: (id: string) => Promise<void>) {
  let pendingId = $state<string | null>(null);
  let executing = false;

  function confirm(id: string) {
    pendingId = id;
  }

  /** Run `onDelete` for the pending id. A call made while one is running returns early. */
  async function execute() {
    if (!pendingId || executing) return;
    executing = true;
    try {
      await onDelete(pendingId);
    } finally {
      executing = false;
      pendingId = null;
    }
  }

  function cancel() {
    pendingId = null;
  }

  return {
    get pendingId() {
      return pendingId;
    },
    confirm,
    execute,
    cancel,
  };
}
