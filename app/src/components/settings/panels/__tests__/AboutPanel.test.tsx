/**
 * Tests for the Settings → About panel.
 *
 * The auto-updater surface is disabled in the closedhuman fork
 * (see `app/src-tauri/tauri.conf.json` + the missing `<AppUpdatePrompt />`
 * mount in `App.tsx`), so the panel collapses to the version display and
 * the Releases link. The Tauri-side `useAppUpdate` hook + its supporting
 * mocks are still in the tree for the day the fork has its own signed
 * release feed.
 */
import { fireEvent, screen } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import { renderWithProviders } from '../../../../test/test-utils';
import AboutPanel from '../AboutPanel';

const hoisted = vi.hoisted(() => ({
  mockOpenUrl: vi.fn(),
}));

const { mockOpenUrl } = hoisted;

vi.mock('../../../../utils/openUrl', () => ({ openUrl: hoisted.mockOpenUrl }));

describe('AboutPanel', () => {
  beforeEach(() => {
    mockOpenUrl.mockReset();
  });

  it('renders the running app version + releases link', () => {
    renderWithProviders(<AboutPanel />);

    // The test config stubs APP_VERSION to '0.0.0-test'.
    expect(screen.getByText('v0.0.0-test')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /Open GitHub releases/ })).toBeInTheDocument();
  });

  it('does NOT surface a "Check for updates" button (auto-update disabled in this fork)', () => {
    renderWithProviders(<AboutPanel />);
    expect(screen.queryByRole('button', { name: /Check for updates/ })).not.toBeInTheDocument();
  });

  it('clicking "Open GitHub releases" calls openUrl with the configured URL', () => {
    renderWithProviders(<AboutPanel />);

    fireEvent.click(screen.getByRole('button', { name: /Open GitHub releases/ }));

    expect(mockOpenUrl).toHaveBeenCalledTimes(1);
    expect(mockOpenUrl.mock.calls[0][0]).toEqual(expect.stringContaining('github.com'));
  });
});
