/**
 * Attributes of every input that takes recovery phrase words: no spell check,
 * no autocorrect, no autofill history, no automatic capitalization. Spread
 * onto the element; `autocorrect` is not in the textarea attribute types.
 */
export const PHRASE_INPUT_ATTRS = {
  spellcheck: "false",
  autocorrect: "off",
  autocomplete: "off",
  autocapitalize: "none",
} as const;
