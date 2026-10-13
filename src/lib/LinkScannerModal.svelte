<script lang="ts">
  import { hapticNotification } from "$lib/haptics";
  import Modal from "$lib/Modal.svelte";
  import QrScanner from "$lib/QrScanner.svelte";
  import type { LinkQrPayload } from "$lib/types";
  import { isPubkeyFormat } from "$lib/utils";

  interface Props {
    /** Called with a payload whose shape and expiry were checked. Nothing is invoked here. */
    onscanned: (payload: LinkQrPayload) => void;
    onclose: () => void;
  }

  let { onscanned, onclose }: Props = $props();

  const uid = $props.id();

  // 32 bytes in base64: 43 characters, with or without one padding character.
  const SECRET = /^[A-Za-z0-9+/_-]{43}=?$/;
  const MAX_RELAY_URL_LENGTH = 2048;

  let error = $state<string | null>(null);
  let manualInput = $state("");
  let showManual = $state(false);

  function isRelayUrl(value: unknown): value is string {
    if (typeof value !== "string" || value.length > MAX_RELAY_URL_LENGTH) {
      return false;
    }
    try {
      const protocol = new URL(value).protocol;
      return protocol === "https:" || protocol === "http:";
    } catch {
      return false;
    }
  }

  /** Returns the payload, or the reason `text` is not accepted. */
  function parsePayload(text: string): LinkQrPayload | string {
    let parsed: unknown;
    try {
      parsed = JSON.parse(text);
    } catch {
      return "Not a device link code.";
    }
    if (typeof parsed !== "object" || parsed === null) {
      return "Not a device link code.";
    }
    const { node_id, secret, relay_url, expires_at } = parsed as Record<
      string,
      unknown
    >;
    if (
      typeof node_id !== "string" ||
      !isPubkeyFormat(node_id) ||
      typeof secret !== "string" ||
      !SECRET.test(secret) ||
      !(
        relay_url === null ||
        relay_url === undefined ||
        isRelayUrl(relay_url)
      ) ||
      typeof expires_at !== "number" ||
      !Number.isSafeInteger(expires_at)
    ) {
      return "Not a device link code.";
    }
    if (expires_at <= Date.now()) {
      return "This link code has expired. Generate a new code on the other device.";
    }
    return { node_id, secret, relay_url: relay_url ?? null, expires_at };
  }

  function submit(text: string, scanned: boolean) {
    const payload = parsePayload(text);
    if (typeof payload === "string") {
      error = payload;
      showManual = true;
      return;
    }
    if (scanned) hapticNotification("success");
    onscanned(payload);
  }
</script>

<QrScanner
  hint="Scan device link QR code"
  onresult={(text) => submit(text, true)}
  onunavailable={() => (showManual = true)}
  oncancel={onclose}
/>

{#if showManual}
  <Modal label="Link with device" {onclose} maxWidth="380px">
    {#if error}
      <p class="scanner-error" role="alert">{error}</p>
    {/if}
    <label class="scanner-label" for="{uid}-payload">
      Paste the link code from your other device:
    </label>
    <textarea
      id="{uid}-payload"
      class="textarea-base manual-input"
      bind:value={manualInput}
      placeholder="JSON with node_id, secret, relay_url, expires_at"
      rows="4"
      spellcheck="false"
      autocomplete="off"
      autocapitalize="none"></textarea>
    <div class="modal-actions">
      <button class="modal-cancel" onclick={onclose}>Cancel</button>
      <button
        class="modal-confirm save"
        onclick={() => submit(manualInput.trim(), false)}
        disabled={!manualInput.trim()}
      >
        Continue
      </button>
    </div>
  </Modal>
{/if}

<style>
  .scanner-label {
    display: block;
    margin: 0 0 0.75rem;
    color: var(--text-secondary);
    font-size: var(--text-sm);
  }

  .scanner-error {
    margin: 0 0 0.75rem;
    color: var(--color-error);
    font-size: var(--text-sm);
  }

  .manual-input {
    width: 100%;
    margin-bottom: 0.75rem;
    font-size: var(--text-xs);
    font-family: monospace;
  }
</style>
