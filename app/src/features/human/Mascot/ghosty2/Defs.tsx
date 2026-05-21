import React from 'react';

import { BODY_PATH } from './paths';

/**
 * SVG `<defs>` for the closedhuman Ghosty 2.0 mascot.
 *
 * Visual language:
 *   * Translucent cool-cyan body, slightly brighter at the top-left so the
 *     character reads as luminous-from-within rather than flat-colored.
 *   * Soft outer glow — gaussian-blur halo around the silhouette so the
 *     ghost appears to emit light onto whatever sits behind it.
 *   * No drop-shadow / ground anchor — ghosts float, they don't cast a
 *     contact shadow.
 *
 * `bodyColor` is the mid-stop of the body gradient and is the only color
 * the caller can override. The highlight and rim tones are derived from it
 * implicitly via opacity layering so the palette stays coherent across
 * themed variants.
 */
export const Ghosty2Defs: React.FC<{
  idPrefix: string;
  bodyColor: string;
  glowColor: string;
}> = ({ idPrefix, bodyColor, glowColor }) => {
  const id = (k: string) => `${idPrefix}-${k}`;
  return (
    <defs>
      <radialGradient id={id('body')} cx="0.34" cy="0.26" r="0.95">
        {/* Bright spectral highlight at the top-left so the body has volume. */}
        <stop offset="0%" stopColor="#ffffff" stopOpacity="0.95" />
        <stop offset="22%" stopColor="#eaf4ff" stopOpacity="0.9" />
        <stop offset="55%" stopColor={bodyColor} stopOpacity="0.82" />
        <stop offset="100%" stopColor={bodyColor} stopOpacity="0.55" />
      </radialGradient>

      <filter id={id('glow')} x="-25%" y="-25%" width="150%" height="150%">
        {/* Outer aura — blur the silhouette wider than itself, tint it the
            glow color, and composite it behind the original. */}
        <feGaussianBlur in="SourceAlpha" stdDeviation="22" result="alphaBlur" />
        <feFlood floodColor={glowColor} floodOpacity="0.45" />
        <feComposite in2="alphaBlur" operator="in" result="glow" />
        <feMerge>
          <feMergeNode in="glow" />
          <feMergeNode in="SourceGraphic" />
        </feMerge>
      </filter>

      <filter id={id('soft')} x="-30%" y="-30%" width="160%" height="160%">
        <feGaussianBlur stdDeviation="28" />
      </filter>

      <clipPath id={id('body-clip')}>
        <path d={BODY_PATH} />
      </clipPath>
    </defs>
  );
};
