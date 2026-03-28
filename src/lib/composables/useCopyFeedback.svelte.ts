import { copyToClipboard } from "$lib/utils";

export function useCopyFeedback() {
  let feedback = $state("");

  async function copy(text: string, label: string) {
    await copyToClipboard(text);
    feedback = label;
    setTimeout(() => (feedback = ""), 1500);
  }

  return {
    get feedback() {
      return feedback;
    },
    copy,
  };
}
