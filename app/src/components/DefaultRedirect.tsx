import { Navigate } from 'react-router-dom';

import { useCoreState } from '../providers/CoreStateProvider';
import { DEV_FORCE_ONBOARDING } from '../utils/config';
import RouteLoadingScreen from './RouteLoadingScreen';

/**
 * Default redirect for unmatched routes.
 *
 * After the local-OAuth refactor there is no user-account auth gate:
 * - Onboarding not completed → /onboarding
 * - Onboarding completed → /home
 */
const DefaultRedirect = () => {
  const { isBootstrapping, snapshot } = useCoreState();

  if (isBootstrapping) {
    return <RouteLoadingScreen />;
  }

  if (DEV_FORCE_ONBOARDING || !snapshot.onboardingCompleted) {
    return <Navigate to="/onboarding" replace />;
  }

  return <Navigate to="/home" replace />;
};

export default DefaultRedirect;
