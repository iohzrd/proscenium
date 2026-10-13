<script lang="ts">
  import { useCopyFeedback } from "$lib/composables";
  import QrCodeDialog from "$lib/QrCodeDialog.svelte";

  interface Props {
    pubkey: string;
    transportNodeId?: string;
    onclose: () => void;
  }

  let { pubkey, transportNodeId, onclose }: Props = $props();

  const copier = useCopyFeedback();

  let deepLinkUrl = $derived(
    transportNodeId
      ? `proscenium://profile/${encodeURIComponent(pubkey)}?transport=${encodeURIComponent(transportNodeId)}`
      : `proscenium://profile/${encodeURIComponent(pubkey)}`,
  );
</script>

<QrCodeDialog
  label="Profile QR code"
  caption="Scan to follow"
  data={deepLinkUrl}
  {onclose}
>
  <div class="qr-url-row">
    <code class="qr-url">{deepLinkUrl}</code>
    <button
      class="btn-elevated copy-link-btn"
      onclick={() => copier.copy(deepLinkUrl, "Copied!")}
    >
      {copier.feedback || "Copy"}
    </button>
  </div>
</QrCodeDialog>

<style>
  .qr-url-row {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    margin-bottom: 1rem;
  }

  .qr-url {
    flex: 1;
    background: var(--bg-deep);
    padding: 0.4rem 0.6rem;
    border-radius: var(--radius-md);
    font-size: var(--text-xs);
    color: var(--color-link);
    word-break: break-all;
    text-align: left;
  }

  .copy-link-btn {
    border-radius: var(--radius-sm);
    padding: 0.4rem 0.6rem;
    font-size: var(--text-sm);
    min-width: 48px;
  }
</style>
