<script lang="ts">
  let {
    checked,
    label,
    disabled = false,
    onchange,
  }: {
    checked: boolean;
    /** Accessible name of the switch. */
    label: string;
    disabled?: boolean;
    /** Called with the new state when the switch is clicked. */
    onchange: (checked: boolean) => void;
  } = $props();
</script>

<button
  class="toggle-switch"
  class:active={checked}
  role="switch"
  aria-checked={checked}
  aria-label={label}
  {disabled}
  onclick={() => onchange(!checked)}
>
  <span class="toggle-knob"></span>
</button>

<style>
  .toggle-switch {
    position: relative;
    width: 44px;
    height: 24px;
    border-radius: 12px;
    border: none;
    background: var(--bg-elevated);
    cursor: pointer;
    flex-shrink: 0;
    padding: 0;
    transition: background 0.2s;
    margin-top: 0.1rem;
  }

  .toggle-switch.active {
    background: var(--accent);
  }

  .toggle-switch:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .toggle-knob {
    position: absolute;
    top: 3px;
    left: 3px;
    width: 18px;
    height: 18px;
    border-radius: 50%;
    background: var(--text-primary);
    transition: transform 0.2s;
  }

  .toggle-switch.active .toggle-knob {
    transform: translateX(20px);
  }
</style>
