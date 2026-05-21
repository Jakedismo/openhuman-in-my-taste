import { render } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { socketService } from '../../services/socketService';
import { useCoreState } from '../CoreStateProvider';
import SocketProvider from '../SocketProvider';

vi.mock('../CoreStateProvider', () => ({ useCoreState: vi.fn() }));

vi.mock('../../services/socketService', () => ({
  socketService: { connect: vi.fn(), disconnect: vi.fn() },
}));

vi.mock('../../hooks/useDaemonLifecycle', () => ({
  useDaemonLifecycle: () => ({
    isAutoStartEnabled: false,
    connectionAttempts: 0,
    isRecovering: false,
    maxAttemptsReached: false,
  }),
}));

const socketStatusMock = vi.hoisted(() => vi.fn<() => string>(() => 'connected'));

vi.mock('react-redux', async () => {
  const actual = await vi.importActual<typeof import('react-redux')>('react-redux');
  return {
    ...actual,
    useSelector: (selector: unknown) => {
      // The provider only calls `useSelector(selectSocketStatus)` —
      // surface whatever the test pinned via `setSocketStatus`.
      void selector;
      return socketStatusMock();
    },
  };
});

vi.mock('../../store/socketSelectors', () => ({
  selectSocketStatus: vi.fn(),
}));

type SnapshotShape = { sessionToken: string | null };

function setToken(token: string | null) {
  vi.mocked(useCoreState).mockReturnValue({
    snapshot: { sessionToken: token } as SnapshotShape,
  } as unknown as ReturnType<typeof useCoreState>);
}

function setSocketStatus(status: string) {
  socketStatusMock.mockImplementation(() => status);
}

// Local-OAuth fork: the previous test suite verified token-gated
// connect behaviour + the now-deleted `openhuman.socket_connect_with_session`
// RPC failure handling. Both surfaces are gone:
//
// 1. Connect-gating-on-token was the chat-blocking bug — there is no
//    session token in this fork, so the socket would never connect
//    and `evaluateComposerSend` returned `blockReason='socket_disconnected'`
//    for every send. The provider now connects on mount with the
//    string `'local'` as a handshake placeholder (the core's
//    Socket.IO server accepts every connection unconditionally and
//    doesn't validate the token).
// 2. The `socket_connect_with_session` RPC was the backend-alphahuman
//    handshake; the OpenHuman backend is dead, so calling it now
//    logs `unknown_method` and spams console errors. The provider
//    no longer calls it.

describe('SocketProvider — local-OAuth connect behaviour', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    // Default to `connected` so the watchdog effect stays dormant.
    // Individual tests flip to `disconnected` to assert the retry
    // loop fires.
    setSocketStatus('connected');
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('connects with a "local" placeholder when mounted with a null token', () => {
    setToken(null);
    render(
      <SocketProvider>
        <div />
      </SocketProvider>
    );

    expect(vi.mocked(socketService.connect)).toHaveBeenCalledTimes(1);
    expect(vi.mocked(socketService.connect)).toHaveBeenCalledWith('local');
  });

  it('connects with the session token when one is present', () => {
    setToken('jwt-abc');
    render(
      <SocketProvider>
        <div />
      </SocketProvider>
    );

    expect(vi.mocked(socketService.connect)).toHaveBeenCalledTimes(1);
    expect(vi.mocked(socketService.connect)).toHaveBeenCalledWith('jwt-abc');
  });

  it('does not reconnect when the same token re-renders', () => {
    setToken('jwt-abc');
    const { rerender } = render(
      <SocketProvider>
        <div />
      </SocketProvider>
    );
    expect(vi.mocked(socketService.connect)).toHaveBeenCalledTimes(1);

    setToken('jwt-abc');
    rerender(
      <SocketProvider>
        <div />
      </SocketProvider>
    );

    expect(vi.mocked(socketService.connect)).toHaveBeenCalledTimes(1);
  });

  it('reconnects when the token rotates to a new value', () => {
    setToken('jwt-first');
    const { rerender } = render(
      <SocketProvider>
        <div />
      </SocketProvider>
    );
    expect(vi.mocked(socketService.connect)).toHaveBeenLastCalledWith('jwt-first');

    setToken('jwt-second');
    rerender(
      <SocketProvider>
        <div />
      </SocketProvider>
    );

    expect(vi.mocked(socketService.connect)).toHaveBeenCalledTimes(2);
    expect(vi.mocked(socketService.connect)).toHaveBeenLastCalledWith('jwt-second');
  });

  it('reconnects with "local" when the token is cleared after being set', () => {
    setToken('jwt-abc');
    const { rerender } = render(
      <SocketProvider>
        <div />
      </SocketProvider>
    );
    expect(vi.mocked(socketService.connect)).toHaveBeenLastCalledWith('jwt-abc');

    setToken(null);
    rerender(
      <SocketProvider>
        <div />
      </SocketProvider>
    );

    expect(vi.mocked(socketService.connect)).toHaveBeenCalledTimes(2);
    expect(vi.mocked(socketService.connect)).toHaveBeenLastCalledWith('local');
  });

  // Watchdog: when socket.io's built-in reconnection caps out and
  // leaves the status at `disconnected` (typical after a core
  // restart that takes >5s — every `pnpm dev:app` cycle in dev),
  // the provider retries `socketService.connect()` every 5s. This
  // closes the "Realtime socket is not connected" composer-block
  // loop the user reported.
  it('watchdog retries socketService.connect() every 5s while status=disconnected', () => {
    vi.useFakeTimers();
    setToken(null);
    setSocketStatus('disconnected');

    render(
      <SocketProvider>
        <div />
      </SocketProvider>
    );

    // Initial mount call.
    expect(vi.mocked(socketService.connect)).toHaveBeenCalledTimes(1);

    vi.advanceTimersByTime(5_000);
    expect(vi.mocked(socketService.connect)).toHaveBeenCalledTimes(2);

    vi.advanceTimersByTime(5_000);
    expect(vi.mocked(socketService.connect)).toHaveBeenCalledTimes(3);
  });

  it('watchdog stays dormant when status is connected', () => {
    vi.useFakeTimers();
    setToken(null);
    setSocketStatus('connected');

    render(
      <SocketProvider>
        <div />
      </SocketProvider>
    );

    expect(vi.mocked(socketService.connect)).toHaveBeenCalledTimes(1);

    vi.advanceTimersByTime(15_000);
    // No additional connect calls — watchdog did not fire.
    expect(vi.mocked(socketService.connect)).toHaveBeenCalledTimes(1);
  });
});
