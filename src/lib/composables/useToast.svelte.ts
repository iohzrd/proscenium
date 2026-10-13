const TOAST_DURATION_MS = 4000;

export function useToast() {
  let message = $state("");
  let type = $state<"error" | "success">("error");
  let timer: ReturnType<typeof setTimeout> | null = null;

  /** Show `msg` for `TOAST_DURATION_MS`, replacing the current message and its timer. */
  function show(msg: string, t: "error" | "success" = "error") {
    message = msg;
    type = t;
    if (timer) clearTimeout(timer);
    timer = setTimeout(() => {
      message = "";
      timer = null;
    }, TOAST_DURATION_MS);
  }

  return {
    get message() {
      return message;
    },
    get type() {
      return type;
    },
    show,
  };
}
