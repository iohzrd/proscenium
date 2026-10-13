import { copyToClipboard } from "$lib/utils";

const FEEDBACK_DURATION_MS = 1500;

export function useCopyFeedback() {
  let feedback = $state("");
  let timer: ReturnType<typeof setTimeout> | null = null;

  /** Copy `text`; on success set `feedback` to `label` for `FEEDBACK_DURATION_MS`. */
  async function copy(text: string, label: string) {
    try {
      await copyToClipboard(text);
    } catch (e) {
      console.error("Clipboard write failed:", e);
      return;
    }
    feedback = label;
    if (timer) clearTimeout(timer);
    timer = setTimeout(() => {
      feedback = "";
      timer = null;
    }, FEEDBACK_DURATION_MS);
  }

  return {
    get feedback() {
      return feedback;
    },
    copy,
  };
}
