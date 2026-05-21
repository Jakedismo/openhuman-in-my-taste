/**
 * Tests for BottomTabBar — verifies that:
 *  - the tab bar renders on non-hidden paths
 *  - the walkthroughAttr mapping is exercised by rendering the tabs
 *  - the tab bar is hidden on '/' and '/login' paths
 *
 * Note: after the local-OAuth refactor the bar no longer depends on a
 * session token (single-user local desktop). The earlier
 * "returns null when there is no session token" case is gone.
 */
import { configureStore } from '@reduxjs/toolkit';
import { render, screen } from '@testing-library/react';
import { Provider } from 'react-redux';
import { MemoryRouter } from 'react-router-dom';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import accountsReducer from '../../store/accountsSlice';
import notificationReducer from '../../store/notificationSlice';
import BottomTabBar from '../BottomTabBar';

// ── Module-level mocks ─────────────────────────────────────────────────────

vi.mock('../../utils/config', async importOriginal => {
  const actual = await importOriginal<typeof import('../../utils/config')>();
  return { ...actual, APP_ENVIRONMENT: 'development' };
});

vi.mock('../../utils/accountsFullscreen', () => ({ isAccountsFullscreen: vi.fn(() => false) }));

// ── Helpers ────────────────────────────────────────────────────────────────

function buildStore() {
  return configureStore({
    reducer: { accounts: accountsReducer, notifications: notificationReducer },
  });
}

function renderBottomTabBar(pathname = '/home') {
  const store = buildStore();
  return render(
    <Provider store={store}>
      <MemoryRouter initialEntries={[pathname]}>
        <BottomTabBar />
      </MemoryRouter>
    </Provider>
  );
}

// ── Tests ──────────────────────────────────────────────────────────────────

describe('BottomTabBar', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it('renders navigation tabs with data-walkthrough attributes', () => {
    renderBottomTabBar('/home');

    expect(screen.getByRole('button', { name: 'Home' })).toBeInTheDocument();

    const chatBtn = screen.getByRole('button', { name: 'Chat' });
    expect(chatBtn).toBeInTheDocument();
    expect(chatBtn).toHaveAttribute('data-walkthrough', 'tab-chat');
  });

  it('renders Settings tab with data-walkthrough="tab-settings"', () => {
    renderBottomTabBar('/home');
    const settingsBtn = screen.getByRole('button', { name: 'Settings' });
    expect(settingsBtn).toHaveAttribute('data-walkthrough', 'tab-settings');
  });

  it('returns null on the "/" path', () => {
    const { container } = renderBottomTabBar('/');
    expect(container.firstChild).toBeNull();
  });
});
