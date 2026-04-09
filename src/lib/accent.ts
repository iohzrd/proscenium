export interface AccentPreset {
  name: string;
  label: string;
  accent: string;
  accentHover: string;
  accentLight: string;
  accentLightHover: string;
  accentMedium: string;
  accentRgb: string; // for rgba() gradients
}

export const ACCENT_PRESETS: Record<string, AccentPreset> = {
  red: {
    name: "red",
    label: "Red",
    accent: "#dc2626",
    accentHover: "#b91c1c",
    accentLight: "#fca5a5",
    accentLightHover: "#fecaca",
    accentMedium: "#f87171",
    accentRgb: "220, 38, 38",
  },
  purple: {
    name: "purple",
    label: "Purple",
    accent: "#7c3aed",
    accentHover: "#6d28d9",
    accentLight: "#c4b5fd",
    accentLightHover: "#e0d4ff",
    accentMedium: "#a78bfa",
    accentRgb: "124, 58, 237",
  },
  blue: {
    name: "blue",
    label: "Blue",
    accent: "#2563eb",
    accentHover: "#1d4ed8",
    accentLight: "#93c5fd",
    accentLightHover: "#bfdbfe",
    accentMedium: "#60a5fa",
    accentRgb: "37, 99, 235",
  },
  green: {
    name: "green",
    label: "Green",
    accent: "#16a34a",
    accentHover: "#15803d",
    accentLight: "#86efac",
    accentLightHover: "#bbf7d0",
    accentMedium: "#4ade80",
    accentRgb: "22, 163, 74",
  },
  orange: {
    name: "orange",
    label: "Orange",
    accent: "#ea580c",
    accentHover: "#c2410c",
    accentLight: "#fdba74",
    accentLightHover: "#fed7aa",
    accentMedium: "#fb923c",
    accentRgb: "234, 88, 12",
  },
  pink: {
    name: "pink",
    label: "Pink",
    accent: "#db2777",
    accentHover: "#be185d",
    accentLight: "#f9a8d4",
    accentLightHover: "#fbcfe8",
    accentMedium: "#f472b6",
    accentRgb: "219, 39, 119",
  },
  teal: {
    name: "teal",
    label: "Teal",
    accent: "#0d9488",
    accentHover: "#0f766e",
    accentLight: "#5eead4",
    accentLightHover: "#99f6e4",
    accentMedium: "#2dd4bf",
    accentRgb: "13, 148, 136",
  },
};

export const DEFAULT_ACCENT = "red";

export function applyAccent(name: string): void {
  const preset = ACCENT_PRESETS[name] ?? ACCENT_PRESETS[DEFAULT_ACCENT];
  const root = document.documentElement;
  root.style.setProperty("--accent", preset.accent);
  root.style.setProperty("--accent-hover", preset.accentHover);
  root.style.setProperty("--accent-light", preset.accentLight);
  root.style.setProperty("--accent-light-hover", preset.accentLightHover);
  root.style.setProperty("--accent-medium", preset.accentMedium);
  root.style.setProperty("--accent-rgb", preset.accentRgb);

  // Derived alpha variants
  root.style.setProperty("--accent-light-faint", preset.accentLight + "40");
  root.style.setProperty("--accent-light-hover-bg", preset.accentLight + "12");
}
