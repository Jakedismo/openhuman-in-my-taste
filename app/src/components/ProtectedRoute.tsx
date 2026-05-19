import { useCoreState } from '../providers/CoreStateProvider';
import RouteLoadingScreen from './RouteLoadingScreen';

interface ProtectedRouteProps {
  children: React.ReactNode;
}

/**
 * Route wrapper that waits for `CoreStateProvider` to finish booting
 * before rendering the page. After the local-OAuth refactor there is
 * no user-account auth gate — single-user local desktop. The wrapper
 * is kept so that pages depending on `useCoreState()` always see a
 * populated snapshot.
 */
const ProtectedRoute = ({ children }: ProtectedRouteProps) => {
  const { isBootstrapping } = useCoreState();

  if (isBootstrapping) {
    return <RouteLoadingScreen />;
  }

  return <>{children}</>;
};

export default ProtectedRoute;
