/**
 * hooks/useOnboarding.tsx
 * Tracks whether the dashboard OnboardingTour has been completed or skipped,
 * persisting that choice in localStorage so it survives a full page reload
 * (#621). Also exposes a way to manually reset the tour (e.g. from Settings)
 * so users can replay it on demand.
 */
import { useCallback, useEffect, useState } from "react";

export const ONBOARDING_STORAGE_KEY = "stellar-micropay:onboarding-completed";

function readOnboardingCompleted(): boolean {
  if (typeof window === "undefined") return false;
  try {
    return window.localStorage.getItem(ONBOARDING_STORAGE_KEY) === "true";
  } catch {
    return false;
  }
}

/**
 * `active` gates the tour on some precondition the caller controls (e.g. a
 * wallet being connected) — the tour is only offered once `active` is true
 * and the user hasn't already completed/skipped it on this device.
 */
export function useOnboarding(active: boolean) {
  const [showTour, setShowTour] = useState(false);

  useEffect(() => {
    if (!active) return;
    if (!readOnboardingCompleted()) {
      setShowTour(true);
    }
  }, [active]);

  const dismissTour = useCallback(() => {
    setShowTour(false);
    try {
      window.localStorage.setItem(ONBOARDING_STORAGE_KEY, "true");
    } catch {
      // Storage disabled — tour will simply re-offer next session.
    }
  }, []);

  return {
    showTour,
    completeTour: dismissTour,
    skipTour: dismissTour,
  };
}

/** Clears the persisted completion flag so the tour is offered again. */
export function resetOnboardingTour(): void {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.removeItem(ONBOARDING_STORAGE_KEY);
  } catch {
    // ignore
  }
}
