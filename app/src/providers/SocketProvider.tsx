import { useEffect, useRef } from 'react';
import { useSelector } from 'react-redux';

import { useDaemonLifecycle } from '../hooks/useDaemonLifecycle';
import { socketService } from '../services/socketService';
import { selectSocketStatus } from '../store/socketSelectors';
import { IS_DEV } from '../utils/config';
import { useCoreState } from './CoreStateProvider';

/**
 * How often the watchdog re-attempts a socket connection when the
 * Redux status reports `disconnected`. `socketService.connect()` is
 * idempotent (skips when already connected with the same token) so
 * polling here is cheap.
 */
const RECONNECT_WATCHDOG_INTERVAL_MS = 5_000;

/**
 * SocketProvider manages the socket connection based on JWT token.
 * The frontend TypeScript socket client is the single realtime path
 * for both desktop and web.
 */
const SocketProvider = ({ children }: { children: React.ReactNode }) => {
  const { snapshot } = useCoreState();
  const token = snapshot.sessionToken;
  const previousTokenRef = useRef<string | null>(null);
  const socketStatus = useSelector(selectSocketStatus);

  // Keep daemon lifecycle management for desktop health/recovery.
  const daemonLifecycle = useDaemonLifecycle();

  useEffect(() => {
    if (IS_DEV) {
      console.log('[SocketProvider] Daemon lifecycle state:', {
        isAutoStartEnabled: daemonLifecycle.isAutoStartEnabled,
        connectionAttempts: daemonLifecycle.connectionAttempts,
        isRecovering: daemonLifecycle.isRecovering,
        maxAttemptsReached: daemonLifecycle.maxAttemptsReached,
      });
    }
  }, [
    daemonLifecycle.isAutoStartEnabled,
    daemonLifecycle.connectionAttempts,
    daemonLifecycle.isRecovering,
    daemonLifecycle.maxAttemptsReached,
  ]);

  // Local-OAuth fork: previously this gated `socketService.connect(token)`
  // on `snapshot.sessionToken`, which doesn't exist in this fork (we
  // deleted the login JWT flow in Phase 5.4). The result was that the
  // socket never connected → `socketStatus !== 'connected'` →
  // `evaluateComposerSend` returned `blockReason='socket_disconnected'`
  // → chat input would not clear and nothing was sent to the core, with
  // only a brief coral-text "Realtime socket is not connected" error
  // flashing under the composer. (User report: "chat still doesn't
  // work, input doesn't even go away from the input bar, nothing
  // special gets logged".)
  //
  // The core's Socket.IO server (`src/core/socketio.rs::attach_socketio`)
  // accepts every connection unconditionally — no token check, the
  // token is purely a frontend-side connect-once cache key. Drop the
  // gate, connect on mount with a static "local" placeholder, and
  // disconnect on unmount.
  useEffect(() => {
    const handshakeToken = token ?? 'local';
    const previousToken = previousTokenRef.current;
    if (handshakeToken === previousToken) {
      return;
    }
    previousTokenRef.current = handshakeToken;
    socketService.connect(handshakeToken);
    // The legacy `openhuman.socket_connect_with_session` RPC connected
    // the Rust core to the backend-alphahuman socket for inbound
    // managed-DM routing. The OpenHuman backend is gone in this fork;
    // calling that method now logs `unknown_method` and produces a
    // noisy "RPC connection failed" console error every launch. Skip
    // it. Channel managed-DM routing for Discord/Telegram is handled
    // natively by the channel listener registry (see
    // `src/openhuman/channels/runtime/listener_registry.rs`).
  }, [token]);

  // Reconnect watchdog. socket.io's built-in reconnection caps at
  // `reconnectionAttempts: 5` × 1 s — so if the core restarts (every
  // `pnpm dev:app` cycle in development; rare in production) and
  // takes longer than 5 s to come back up, the socket gives up and
  // sits at `disconnected` forever. The user then sees "Realtime
  // socket is not connected — responses cannot be delivered without
  // a client ID." on every subsequent send and has to refresh the
  // app. This watchdog re-fires `socketService.connect()` every 5 s
  // while the Redux status is `disconnected`. The service's connect
  // is idempotent (early-returns when already connected with the
  // same token), so the cost when already connected is a single
  // identity check per tick.
  useEffect(() => {
    if (socketStatus !== 'disconnected') {
      return;
    }
    const handshakeToken = token ?? 'local';
    const intervalId = window.setInterval(() => {
      if (IS_DEV) {
        console.log('[SocketProvider] watchdog retrying socket connect (status=disconnected)');
      }
      socketService.connect(handshakeToken);
    }, RECONNECT_WATCHDOG_INTERVAL_MS);
    return () => {
      window.clearInterval(intervalId);
    };
  }, [socketStatus, token]);

  // Cleanup on unmount only
  useEffect(() => {
    return () => {
      const currentToken = snapshot.sessionToken;
      if (!currentToken) {
        socketService.disconnect();
      }
    };
  }, [snapshot.sessionToken]);

  return <>{children}</>;
};

export default SocketProvider;
