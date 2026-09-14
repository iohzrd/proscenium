export interface AccentPreset {
  name: string;
  label: string;
  accent: string;
  accentHover: string;
  accentLight: string;
  accentLightHover: string;
  accentMedium: string;
}

// Desaturated, warm-leaning accents. Keys are persisted by the backend, so
// the names stay stable while the values change.
export const ACCENT_PRESETS: Record<string, AccentPreset> = {
  red: {
    name: "red",
    label: "Terracotta",
    accent: "#c24b3f",
    accentHover: "#a83f35",
    accentLight: "#e9a89f",
    accentLightHover: "#f2c7c1",
    accentMedium: "#dc7d72",
  },
  purple: {
    name: "purple",
    label: "Heather",
    accent: "#8a6fc4",
    accentHover: "#785fae",
    accentLight: "#c9bae8",
    accentLightHover: "#ded4f2",
    accentMedium: "#ab97d8",
  },
  blue: {
    name: "blue",
    label: "Slate Blue",
    accent: "#4f7fc0",
    accentHover: "#436ea9",
    accentLight: "#a9c2e4",
    accentLightHover: "#c5d6ee",
    accentMedium: "#7ca0d4",
  },
  green: {
    name: "green",
    label: "Moss",
    accent: "#5f9a5a",
    accentHover: "#52874e",
    accentLight: "#b4d4ae",
    accentLightHover: "#cce2c8",
    accentMedium: "#88b982",
  },
  orange: {
    name: "orange",
    label: "Ochre",
    accent: "#d0853a",
    accentHover: "#b97432",
    accentLight: "#eec69c",
    accentLightHover: "#f4d9bb",
    accentMedium: "#e2a666",
  },
  pink: {
    name: "pink",
    label: "Rose",
    accent: "#c4628c",
    accentHover: "#ad547b",
    accentLight: "#e9b3c8",
    accentLightHover: "#f1cbda",
    accentMedium: "#d78aab",
  },
  teal: {
    name: "teal",
    label: "Verdigris",
    accent: "#40978f",
    accentHover: "#37847d",
    accentLight: "#a4d1cc",
    accentLightHover: "#c0e0dc",
    accentMedium: "#70b6af",
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

  // Derived alpha variants
  root.style.setProperty("--accent-light-faint", preset.accentLight + "40");
  root.style.setProperty("--accent-light-hover-bg", preset.accentLight + "12");
}
