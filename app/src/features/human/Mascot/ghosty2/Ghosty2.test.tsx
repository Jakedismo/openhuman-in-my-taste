import { render } from '@testing-library/react';
import { describe, expect, it } from 'vitest';

import type { MascotFace } from '../Ghosty';
import { VISEMES } from '../visemes';
import { Ghosty2 } from './Ghosty2';

const FACES: MascotFace[] = [
  'sleep',
  'idle',
  'normal',
  'listening',
  'thinking',
  'confused',
  'speaking',
  'happy',
  'concerned',
];

describe('Ghosty2', () => {
  it.each(FACES)('renders the %s face preset without crashing', face => {
    const { container } = render(<Ghosty2 face={face} />);
    const svg = container.querySelector('svg[data-mascot="ghosty2"]');
    expect(svg).not.toBeNull();
    expect(svg!.getAttribute('data-face')).toBe(face);
  });

  it('tags the SVG with data-mascot="ghosty2" so it is distinguishable from the upstream YellowMascot', () => {
    const { container } = render(<Ghosty2 face="idle" />);
    expect(container.querySelector('svg[data-mascot="ghosty2"]')).not.toBeNull();
    // And it must NOT pretend to be the upstream component — assert by
    // looking for a known upstream-only feature (blush ellipses).
    // Ghosty 2.0 has no blush.
    const blushish = container.querySelectorAll('ellipse[opacity]');
    for (const el of Array.from(blushish)) {
      const fill = el.getAttribute('fill') ?? '';
      // The upstream Ghosty uses #f4a3a3-tinted blush ellipses; assert
      // none of our ellipses carry that pink tint.
      expect(fill).not.toMatch(/#f4a3a3/i);
    }
  });

  it('renders eyebrows for focus/worry states', () => {
    for (const face of ['listening', 'thinking', 'confused', 'concerned'] as MascotFace[]) {
      const { container } = render(<Ghosty2 face={face} />);
      expect(container.querySelector(`g[data-face-brows="${face}"]`)).not.toBeNull();
    }
  });

  it('omits eyebrows for neutral / acknowledgement states', () => {
    for (const face of ['sleep', 'idle', 'normal', 'speaking', 'happy'] as MascotFace[]) {
      const { container } = render(<Ghosty2 face={face} />);
      expect(container.querySelector('g[data-face-brows]')).toBeNull();
    }
  });

  it('renders a viseme-driven mouth when speaking, distinct from the rest mouth', () => {
    const { container: speaking } = render(
      <Ghosty2 face="speaking" viseme={VISEMES.A} idPrefix="m1" />
    );
    const { container: idle } = render(<Ghosty2 face="idle" idPrefix="m2" />);
    const speakingMouth = speaking.querySelector('path[data-face="speaking"]')?.getAttribute('d');
    const idleMouth = idle.querySelector('path[data-face="idle"]')?.getAttribute('d');
    expect(speakingMouth).toBeTruthy();
    expect(idleMouth).toBeTruthy();
    expect(speakingMouth).not.toBe(idleMouth);
  });

  it('renders an outer-glow group so the body reads as luminous', () => {
    const { container } = render(<Ghosty2 face="idle" />);
    expect(container.querySelector('g[data-face-glow="idle"]')).not.toBeNull();
  });

  it('namespaces SVG defs by idPrefix so multiple mascots can coexist on one page', () => {
    const { container } = render(
      <div>
        <Ghosty2 face="idle" idPrefix="a" />
        <Ghosty2 face="happy" idPrefix="b" />
      </div>
    );
    expect(container.querySelector('#a-body')).not.toBeNull();
    expect(container.querySelector('#b-body')).not.toBeNull();
    expect(container.querySelector('#a-glow')).not.toBeNull();
    expect(container.querySelector('#b-glow')).not.toBeNull();
  });
});
