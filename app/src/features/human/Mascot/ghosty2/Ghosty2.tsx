import React from 'react';

import type { MascotFace } from '../Ghosty';
import { useMascotClock } from '../useMascotClock';
import { visemePath, VISEMES, type VisemeShape } from '../visemes';
import { Ghosty2Defs } from './Defs';
import { BODY_PATH, EYE_CY, EYE_LEFT_CX, EYE_RIGHT_CX, VIEWBOX } from './paths';

export interface Ghosty2Props {
  /**
   * Mid-stop of the body radial gradient. Default is a cool cyan-white that
   * reads as a luminous ghost on dark backgrounds.
   */
  bodyColor?: string;
  /**
   * Outer-glow tint. Defaults to the same cool cyan so the body and glow
   * read as one luminous body, but can be themed separately for accent
   * states (e.g. warm amber on `concerned`).
   */
  glowColor?: string;
  /** High-level state from the agent/voice lifecycle. */
  face?: MascotFace;
  /** Active mouth shape. When omitted, the mouth rests in a closed shape. */
  viseme?: VisemeShape;
  /** Override SVG element size; defaults to filling the parent. */
  size?: number | string;
  /** Prefix for in-SVG `<defs>` ids so multiple mascots can coexist. */
  idPrefix?: string;
}

interface FacePreset {
  eyeScaleY: number;
  eyeScaleX: number;
  browTilt: number;
  browDy: number;
  showBrows: boolean;
  /** Multiplier on the outer glow opacity. Calm states glow softer. */
  glowIntensity: number;
}

const FACE_PRESETS: Record<Exclude<MascotFace, 'normal'>, FacePreset> = {
  sleep: {
    eyeScaleY: 0.1,
    eyeScaleX: 1,
    browTilt: 0,
    browDy: 2,
    showBrows: false,
    glowIntensity: 0.45,
  },
  idle: {
    eyeScaleY: 1,
    eyeScaleX: 1,
    browTilt: 0,
    browDy: 0,
    showBrows: false,
    glowIntensity: 0.7,
  },
  listening: {
    eyeScaleY: 1.05,
    eyeScaleX: 1.05,
    browTilt: -8,
    browDy: -10,
    showBrows: true,
    glowIntensity: 0.9,
  },
  thinking: {
    eyeScaleY: 0.7,
    eyeScaleX: 1,
    browTilt: -4,
    browDy: -2,
    showBrows: true,
    glowIntensity: 0.6,
  },
  confused: {
    eyeScaleY: 0.85,
    eyeScaleX: 0.95,
    browTilt: 14,
    browDy: -4,
    showBrows: true,
    glowIntensity: 0.6,
  },
  speaking: {
    eyeScaleY: 1,
    eyeScaleX: 1,
    browTilt: 0,
    browDy: 0,
    showBrows: false,
    glowIntensity: 1.0,
  },
  happy: {
    eyeScaleY: 0.45,
    eyeScaleX: 1.1,
    browTilt: -6,
    browDy: -6,
    showBrows: false,
    glowIntensity: 1.0,
  },
  concerned: {
    eyeScaleY: 0.95,
    eyeScaleX: 0.95,
    browTilt: 22,
    browDy: -2,
    showBrows: true,
    glowIntensity: 0.5,
  },
};

function presetFor(face: MascotFace): FacePreset {
  return FACE_PRESETS[face === 'normal' ? 'idle' : face];
}

/**
 * Closedhuman mascot — "Ghosty 2.0".
 *
 * Distinct from the upstream YellowMascot:
 *   * Single luminous translucent body — no separate head-dot, no arms,
 *     no humanoid legs. The bottom edge dissolves into wispy scallops.
 *   * Cool cyan-white palette with an outer glow filter; reads as
 *     ghost-in-your-machine rather than the cute yellow blob.
 *   * Same `MascotFace` API + viseme system so lipsync, blink, and the
 *     agent/voice lifecycle wiring stay unchanged.
 */
export const Ghosty2: React.FC<Ghosty2Props> = ({
  bodyColor = '#9ec8e8',
  glowColor = '#7fb4dc',
  face = 'idle',
  viseme,
  size = '100%',
  idPrefix = 'mascot-ghosty2',
}) => {
  const t = useMascotClock();
  const preset = presetFor(face);

  // Gentle floating bob — slower than the upstream blob's bob because a
  // ghost should drift, not bounce.
  const bob = Math.sin(t * Math.PI * 0.9) * 16;
  const sway = Math.sin(t * Math.PI * 0.5 + 1.3) * 6;

  // Blink ~0.2s every 2.6s; slow it down a touch while `thinking`.
  const blinkMs = face === 'thinking' ? 4200 : 2600;
  const blinkOffset = blinkMs / 2;
  const tMs = t * 1000;
  const inBlink = (tMs + blinkOffset) % blinkMs < 200;
  const blinkScale = inBlink ? 0.12 : 1;

  const id = (k: string) => `${idPrefix}-${k}`;
  const bodyFill = `url(#${id('body')})`;
  const glowFilter = `url(#${id('glow')})`;
  const softFilter = `url(#${id('soft')})`;

  const restMouth = restMouthPath(face);

  return (
    <svg
      width={size}
      height={size}
      viewBox={`0 0 ${VIEWBOX} ${VIEWBOX}`}
      style={{ overflow: 'visible', display: 'block' }}
      data-face={face}
      data-mascot="ghosty2">
      <Ghosty2Defs idPrefix={idPrefix} bodyColor={bodyColor} glowColor={glowColor} />

      <g transform={`translate(${sway}, ${bob})`}>
        {/* Outer glow + body silhouette in one group so the filter aura
            stays attached to the silhouette across animation. */}
        <g
          filter={glowFilter}
          opacity={preset.glowIntensity}
          data-face-glow={face}>
          <path d={BODY_PATH} fill={bodyFill} />
        </g>

        {/* Inner highlights, clipped to the body so they don't leak past
            the wispy tail. */}
        <g clipPath={`url(#${id('body-clip')})`}>
          <g filter={softFilter}>
            <ellipse cx={320} cy={340} rx={210} ry={150} fill="#ffffff" opacity={0.18} />
            <ellipse cx={700} cy={780} rx={260} ry={160} fill={glowColor} opacity={0.22} />
          </g>
        </g>

        {preset.showBrows && (
          <g fill="#0a1a2a" data-face-brows={face}>
            <rect
              x={385}
              y={455 + preset.browDy}
              width={60}
              height={9}
              rx={4}
              transform={`rotate(${-preset.browTilt} 415 ${460 + preset.browDy})`}
            />
            <rect
              x={595}
              y={455 + preset.browDy}
              width={60}
              height={9}
              rx={4}
              transform={`rotate(${preset.browTilt} 625 ${460 + preset.browDy})`}
            />
          </g>
        )}

        <g>
          <ellipse
            cx={EYE_LEFT_CX}
            cy={EYE_CY}
            rx={30 * preset.eyeScaleX}
            ry={40 * preset.eyeScaleY * blinkScale}
            fill="#0a1a2a"
          />
          <ellipse
            cx={EYE_RIGHT_CX}
            cy={EYE_CY}
            rx={30 * preset.eyeScaleX}
            ry={40 * preset.eyeScaleY * blinkScale}
            fill="#0a1a2a"
          />
          {!inBlink && (
            <>
              {/* Catchlight — small white dot offset up-and-left from each
                  pupil so the ghost reads as alert, not vacant. */}
              <circle cx={EYE_LEFT_CX + 10} cy={EYE_CY - 14} r={7} fill="#ffffff" />
              <circle cx={EYE_RIGHT_CX + 10} cy={EYE_CY - 14} r={7} fill="#ffffff" />
            </>
          )}
        </g>

        {face === 'speaking' ? (
          <path d={visemePath(viseme ?? VISEMES.REST)} fill="#0a1a2a" data-face={face} />
        ) : (
          <path d={restMouth} fill="#0a1a2a" data-face={face} />
        )}
      </g>
    </svg>
  );
};

/**
 * Closed-mouth shape for non-speaking states. Same mouth vocabulary as the
 * upstream Ghosty so the face-preset → mouth mapping carries over.
 */
function restMouthPath(face: MascotFace): string {
  switch (face) {
    case 'sleep':
      return 'M496,588 Q520,593 544,588 Q520,592 496,588 Z';
    case 'happy':
      return 'M460,565 Q520,635 580,565 Q520,605 460,565 Z';
    case 'concerned':
      return 'M478,605 Q520,560 562,605 Q520,590 478,605 Z';
    case 'confused':
      return 'M478,580 Q520,610 562,575 Q520,597 478,580 Z';
    case 'thinking':
      return 'M488,585 Q520,595 552,585 Q520,592 488,585 Z';
    case 'listening':
      return 'M495,580 Q520,600 545,580 Q520,615 495,580 Z';
    default:
      return visemePath(VISEMES.REST);
  }
}
