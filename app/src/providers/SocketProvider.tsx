import { useEffect, useRef } from 'react';

import { useDaemonLifecycle } from '../hooks/useDaemonLifecycle';
import { socketService } from '../services/socketService';
import { IS_DEV } from '../utils/config';
import { useCoreState } from './CoreStateProvider';

/**
 * SocketProvider manages the socket connection based on JWT token.
 * The frontend TypeScript socket client is the single realtime path
 * for both desktop and web.
 */
const SocketProvider = ({ children }: { children: React.ReactNode }) => {
  const { snapshot } = useCoreState();
  const token = snapshot.sessionToken;
  const previousTokenRef = useRef<string | null>(null);

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
