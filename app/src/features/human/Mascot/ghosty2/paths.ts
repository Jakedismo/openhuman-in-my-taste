// SVG path constants for the closedhuman Ghosty 2.0 mascot.
//
// Classic ghost silhouette in a 1000x1000 viewBox:
//   * round dome head from (170, 560) over to (830, 560) via (500, 110)
//   * straight body sides from y=560 down to y=800
//   * six wispy scallops along the bottom edge — the identity-defining
//     differentiator from the upstream yellow-blob mascot which uses
//     separate humanoid leg paths
//
// The body is intentionally a single closed shape (no separate legs / arms /
// head-dot), so the whole character reads as one luminous ghost.

export const BODY_PATH =
  'M170,560 ' +
  // Round dome head, left half then right half.
  'C170,320 300,110 500,110 ' +
  'C700,110 830,320 830,560 ' +
  // Straight body sides down to the tail line.
  'L830,800 ' +
  // Six scalloped wisps. Each cubic dips to y≈860 between two peaks at y=800.
  // Wisps alternate so the silhouette looks hand-drawn, not mechanical.
  'C815,866 770,866 745,800 ' +
  'C720,860 660,860 625,800 ' +
  'C590,866 540,866 500,800 ' +
  'C460,860 400,860 375,800 ' +
  'C340,866 280,866 255,800 ' +
  'C230,860 190,860 170,800 ' +
  // Close back up the left side to the start of the dome.
  'Z';

// Eye coordinates (kept compatible with the upstream Ghosty layout so the
// viseme/face-preset math doesn't need to shift). Eye centers at (415, 515)
// and (625, 515) match the original.
export const EYE_LEFT_CX = 415;
export const EYE_RIGHT_CX = 625;
export const EYE_CY = 515;

// Mouth anchor — viseme paths are authored against the same x≈520 / y≈590
// region as the upstream mouth, so the `visemes` module stays reusable.
export const MOUTH_ANCHOR_X = 520;
export const MOUTH_ANCHOR_Y = 590;

export const VIEWBOX = 1000;
