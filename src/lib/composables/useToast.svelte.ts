export function useToast() {
  let message = $state("");
  let type = $state<"error" | "success">("error");

  function show(msg: string, t: "error" | "success" = "error") {
    message = msg;
    type = t;
    setTimeout(() => (message = ""), 4000);
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
