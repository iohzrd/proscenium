export function useMentionAutocomplete(
  getContent: () => string,
  setContent: (v: string) => void,
  textareaSelector: string,
) {
  let query = $state("");
  let active = $state(false);

  function handleInput(e: Event) {
    const textarea = e.target as HTMLTextAreaElement;
    const cursorPos = textarea.selectionStart;
    const textBeforeCursor = textarea.value.slice(0, cursorPos);
    const match = textBeforeCursor.match(/@(\w*)$/);
    if (match) {
      active = true;
      query = match[1];
    } else {
      active = false;
      query = "";
    }
  }

  function insertMention(pubkey: string) {
    const textarea = document.querySelector(
      textareaSelector,
    ) as HTMLTextAreaElement;
    const cursorPos = textarea.selectionStart;
    const content = getContent();
    const textBeforeCursor = content.slice(0, cursorPos);
    const textAfterCursor = content.slice(cursorPos);
    const match = textBeforeCursor.match(/@(\w*)$/);
    if (match) {
      const beforeMention = textBeforeCursor.slice(0, match.index);
      setContent(`${beforeMention}@${pubkey} ${textAfterCursor}`);
    }
    active = false;
    query = "";
    textarea.focus();
  }

  return {
    get query() {
      return query;
    },
    get active() {
      return active;
    },
    handleInput,
    insertMention,
  };
}
