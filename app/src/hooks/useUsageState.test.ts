import { act, renderHook } from '@testing-library/react';
import { describe, expect, it } from 'vitest';

import { useUsageState } from './useUsageState';

// Local-OAuth fork: the team/billing/credits RPCs were deleted in
// Phase 5.3. The hook no longer makes any network calls — it returns
// a stable empty/free state so its UI consumers (`TokenUsagePill`,
// `GlobalUpsellBanner`, `Conversations`, `Home`) stay dormant. The
// previous test suite mocked the now-deleted billingApi / creditsApi
// modules and asserted budget-exhaustion + rate-limit math against
// fixture rows; those assertions are obsolete because the underlying
// RPC surface is gone. The replacement tests below pin the new
// invariants: the hook is a pure component-time constant + a refresh
// shim, and emits no errors.

describe('useUsageState (local-OAuth fork)', () => {
  it('returns a stable FREE-tier empty state without making any RPC calls', () => {
    const { result } = renderHook(() => useUsageState());

    expect(result.current.teamUsage).toBeNull();
    expect(result.current.currentPlan).toBeNull();
    expect(result.current.currentTier).toBe('FREE');
    expect(result.current.isFreeTier).toBe(true);
    expect(result.current.usagePct10h).toBe(0);
    expect(result.current.usagePct7d).toBe(0);
    expect(result.current.isNearLimit).toBe(false);
    expect(result.current.isAtLimit).toBe(false);
    expect(result.current.isRateLimited).toBe(false);
    expect(result.current.isBudgetExhausted).toBe(false);
    expect(result.current.shouldShowBudgetCompletedMessage).toBe(false);
    expect(result.current.isLoading).toBe(false);
  });

  it('exposes a refresh() that re-runs without throwing', () => {
    const { result } = renderHook(() => useUsageState());

    expect(typeof result.current.refresh).toBe('function');
    act(() => {
      result.current.refresh();
    });
    // State is still the stable empty values after the refresh.
    expect(result.current.currentTier).toBe('FREE');
    expect(result.current.isRateLimited).toBe(false);
  });
});
