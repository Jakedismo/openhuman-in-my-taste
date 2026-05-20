import { useCallback, useEffect, useState } from 'react';

import type { TeamUsage } from '../services/api/creditsApi';
import type { CurrentPlanData, PlanTier } from '../types/api';
import { subscribeUsageRefresh } from './usageRefresh';

export interface UsageState {
  teamUsage: TeamUsage | null;
  currentPlan: CurrentPlanData | null;
  currentTier: PlanTier;
  isFreeTier: boolean;
  usagePct10h: number;
  usagePct7d: number;
  isNearLimit: boolean;
  isAtLimit: boolean;
  isRateLimited: boolean;
  isBudgetExhausted: boolean;
  shouldShowBudgetCompletedMessage: boolean;
  isLoading: boolean;
  refresh: () => void;
}

// Local-OAuth fork: the team/billing/credits domains were deleted in
// Phase 5.3 (no user accounts, no metered cloud billing). The RPCs
// `openhuman.team_get_usage` and `openhuman.billing_get_current_plan`
// no longer exist, so calling them produces back-to-back warnings:
//   [rpc:dispatch] unknown_method method=openhuman.team_get_usage
//   [observability] rpc.invoke_method failed: unknown method
// every time a consumer (`TokenUsagePill`, `GlobalUpsellBanner`,
// `Conversations`, `Home`) renders. The hook's consumers all
// null-safe their UI when both legs are null — the cleanest fix is
// to skip the RPCs entirely. A future Phase 5.4 follow-up will
// delete this hook and its dead consumers wholesale; for now we
// return a stable empty state and `currentTier = 'FREE'` so any
// "near limit" / "rate limited" banners stay dormant.
export function useUsageState(): UsageState {
  const [, setFetchCount] = useState(0);

  const refresh = useCallback(() => {
    setFetchCount(c => c + 1);
  }, []);

  useEffect(() => subscribeUsageRefresh(refresh), [refresh]);

  return {
    teamUsage: null,
    currentPlan: null,
    currentTier: 'FREE',
    isFreeTier: true,
    usagePct10h: 0,
    usagePct7d: 0,
    isNearLimit: false,
    isAtLimit: false,
    isRateLimited: false,
    isBudgetExhausted: false,
    shouldShowBudgetCompletedMessage: false,
    isLoading: false,
    refresh,
  };
}
