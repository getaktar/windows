import { createDarkTheme, createLightTheme, type BrandVariants, type Theme } from "@fluentui/react-components";

/** The Mac app's accent color (AccentColor in its asset catalog). */
const ACCENT = [0x12, 0x5e, 0xfe] as const;

function mix(color: readonly number[], target: number, amount: number) {
  return color.map((channel) => Math.round(channel + (target - channel) * amount));
}

function hex(color: number[]) {
  return `#${color.map((channel) => channel.toString(16).padStart(2, "0")).join("")}`;
}

/**
 * Fluent's 16-step brand ramp with the accent at step 80, the shade Fluent
 * uses for primary buttons in the light theme: darker toward 10, lighter
 * toward 160.
 */
function brandRamp(): BrandVariants {
  const steps = [10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120, 130, 140, 150, 160] as const;
  const entries = steps.map((step) => {
    if (step < 80) return [step, hex(mix(ACCENT, 0, ((80 - step) / 70) * 0.85))];
    if (step > 80) return [step, hex(mix(ACCENT, 255, ((step - 80) / 80) * 0.9))];
    return [step, hex([...ACCENT])];
  });
  return Object.fromEntries(entries) as BrandVariants;
}

const brand = brandRamp();

export const lightTheme: Theme = {
  ...createLightTheme(brand),
  fontFamilyBase: "'Segoe UI Variable Text', 'Segoe UI', system-ui, sans-serif",
};

export const darkTheme: Theme = {
  ...createDarkTheme(brand),
  fontFamilyBase: "'Segoe UI Variable Text', 'Segoe UI', system-ui, sans-serif",
  // Fluent's guidance for dark themes: lighter brand shades for text and
  // links so they stay readable on dark backgrounds.
  colorBrandForeground1: brand[110],
  colorBrandForeground2: brand[120],
  colorBrandForegroundLink: brand[110],
  colorBrandForegroundLinkHover: brand[120],
  colorBrandForegroundLinkPressed: brand[100],
  colorCompoundBrandForeground1: brand[110],
  colorCompoundBrandStroke: brand[110],
};
