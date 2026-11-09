<script lang="ts">
  interface Props {
    timestamp: number;
  }

  let { timestamp }: Props = $props();

  const SECOND = 1000;
  const MINUTE = 60 * SECOND;
  const HOUR = 60 * MINUTE;
  const DAY = 24 * HOUR;

  /** Units from largest to smallest, with their length in ms. */
  const UNITS: [Intl.RelativeTimeFormatUnit, number][] = [
    ["year", 365 * DAY],
    ["month", 30 * DAY],
    ["week", 7 * DAY],
    ["day", DAY],
    ["hour", HOUR],
    ["minute", MINUTE],
    ["second", SECOND],
  ];

  const relative = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" });

  /** `timestamp` relative to now in the largest whole unit, e.g. "3 minutes ago". */
  function format(timestamp: number): string {
    const delta = timestamp - Date.now();
    const [unit, length] =
      UNITS.find(([, length]) => Math.abs(delta) >= length) ??
      UNITS[UNITS.length - 1];
    return relative.format(Math.trunc(delta / length), unit);
  }

  let timeago: string = $state("");
  let datetime = $derived(new Date(timestamp).toLocaleString());

  $effect(() => {
    // Re-read timestamp to track it as a dependency
    timeago = format(timestamp);

    function tick() {
      timeago = format(timestamp);
      const delta = Date.now() - timestamp;
      let delay: number;
      if (delta < MINUTE) {
        delay = SECOND;
      } else if (delta < HOUR) {
        delay = MINUTE;
      } else {
        delay = HOUR;
      }
      timer = setTimeout(tick, delay);
    }

    let timer = setTimeout(tick, SECOND);
    return () => clearTimeout(timer);
  });
</script>

<span title={datetime}>{timeago}</span>
