export function useDeleteConfirm(onDelete: (id: string) => Promise<void>) {
  let pendingId = $state<string | null>(null);

  function confirm(id: string) {
    pendingId = id;
  }

  async function execute() {
    if (!pendingId) return;
    await onDelete(pendingId);
    pendingId = null;
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
