import { screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';

import { renderWithProviders } from '../../test/test-utils';
import ConnectionIndicator from '../ConnectionIndicator';

describe('ConnectionIndicator', () => {
  it('renders connected state with override prop', () => {
    renderWithProviders(<ConnectionIndicator status="connected" />);
    expect(screen.getByText(/Connected to OpenHuman AI/)).toBeInTheDocument();
  });

  it('renders disconnected state', () => {
    renderWithProviders(<ConnectionIndicator status="disconnected" />);
    expect(screen.getByText('Disconnected')).toBeInTheDocument();
  });

  it('renders connecting state', () => {
    renderWithProviders(<ConnectionIndicator status="connecting" />);
    expect(screen.getByText('Connecting')).toBeInTheDocument();
  });

  it('renders as a pill badge', () => {
    renderWithProviders(<ConnectionIndicator status="connected" />);
    // The indicator renders as an inline pill — status text is visible
    expect(screen.getByText(/Connected to OpenHuman AI/)).toBeInTheDocument();
  });

  it('falls back to connectivity store when no override', () => {
    // Default connectivity state: internet online + core unknown +
    // backend connecting. In the local-OAuth fork the backend socket
    // is no longer a blocking state, so the indicator stays on
    // "Connected to OpenHuman AI" once the other gates are green.
    renderWithProviders(<ConnectionIndicator />);
    expect(screen.getByText(/Connected to OpenHuman AI/)).toBeInTheDocument();
  });

  // ---- Store-driven branches (lines 43, 50, 57, 67) ----

  it('shows "Connected to OpenHuman AI" when blocking=ok (line 43)', () => {
    renderWithProviders(<ConnectionIndicator />, {
      preloadedState: {
        connectivity: {
          internet: 'online',
          core: 'reachable',
          backend: 'connected',
          lastError: {},
        },
      },
    });
    expect(screen.getByText(/Connected to OpenHuman AI/)).toBeInTheDocument();
  });

  it('shows "Offline" when blocking=internet-offline (line 50)', () => {
    renderWithProviders(<ConnectionIndicator />, {
      preloadedState: {
        connectivity: {
          internet: 'offline',
          core: 'reachable',
          backend: 'connected',
          lastError: {},
        },
      },
    });
    expect(screen.getByText('Offline')).toBeInTheDocument();
  });

  it('shows "Core offline" when blocking=core-unreachable (line 57)', () => {
    renderWithProviders(<ConnectionIndicator />, {
      preloadedState: {
        connectivity: {
          internet: 'online',
          core: 'unreachable',
          backend: 'connected',
          lastError: {},
        },
      },
    });
    expect(screen.getByText('Core offline')).toBeInTheDocument();
  });

  it('stays "Connected" when only the backend socket is disconnected (local-OAuth fork)', () => {
    // The hosted OpenHuman backend was removed — `backend-only` is no
    // longer a blocking state, so a disconnected backend channel must
    // not surface "Reconnecting…" in the indicator.
    renderWithProviders(<ConnectionIndicator />, {
      preloadedState: {
        connectivity: {
          internet: 'online',
          core: 'reachable',
          backend: 'disconnected',
          lastError: {},
        },
        socket: { byUser: {} },
      },
    });
    expect(screen.getByText(/Connected to OpenHuman AI/)).toBeInTheDocument();
  });

  it('stays "Connected" when only the backend socket is connecting (local-OAuth fork)', () => {
    renderWithProviders(<ConnectionIndicator />, {
      preloadedState: {
        connectivity: {
          internet: 'online',
          core: 'reachable',
          backend: 'connecting',
          lastError: {},
        },
        socket: { byUser: { __pending__: { status: 'connecting', socketId: null } } },
      },
    });
    expect(screen.getByText(/Connected to OpenHuman AI/)).toBeInTheDocument();
  });
});
