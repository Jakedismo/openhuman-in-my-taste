import { render, screen } from '@testing-library/react';
import { MemoryRouter, Route, Routes } from 'react-router-dom';
import { describe, expect, it, vi } from 'vitest';

import DefaultRedirect from '../DefaultRedirect';

vi.mock('../../utils/config', () => ({ DEV_FORCE_ONBOARDING: false }));

const mockUseCoreState = vi.fn();
vi.mock('../../providers/CoreStateProvider', () => ({ useCoreState: () => mockUseCoreState() }));

function renderRedirect(initialEntry = '*') {
  return render(
    <MemoryRouter initialEntries={[`/${initialEntry}`]}>
      <Routes>
        <Route path="/" element={<div>Welcome</div>} />
        <Route path="/onboarding" element={<div>Onboarding</div>} />
        <Route path="/home" element={<div>Home</div>} />
        <Route path="*" element={<DefaultRedirect />} />
      </Routes>
    </MemoryRouter>
  );
}

describe('DefaultRedirect', () => {
  it('shows loading while bootstrapping', () => {
    mockUseCoreState.mockReturnValue({
      isBootstrapping: true,
      snapshot: { sessionToken: null, currentUser: null, onboardingCompleted: false },
    });

    renderRedirect();

    expect(screen.queryByText('Welcome')).not.toBeInTheDocument();
    expect(screen.queryByText('Onboarding')).not.toBeInTheDocument();
    expect(screen.queryByText('Home')).not.toBeInTheDocument();
  });

  it('redirects to /onboarding when onboarding has not been completed', () => {
    // In the local-OAuth fork there is no user-account auth gate;
    // onboarding state alone decides between /onboarding and /home.
    mockUseCoreState.mockReturnValue({
      isBootstrapping: false,
      snapshot: { onboardingCompleted: false },
    });

    renderRedirect();

    expect(screen.getByText('Onboarding')).toBeInTheDocument();
  });

  it('redirects to /home when onboarding has been completed', () => {
    mockUseCoreState.mockReturnValue({
      isBootstrapping: false,
      snapshot: { onboardingCompleted: true },
    });

    renderRedirect();

    expect(screen.getByText('Home')).toBeInTheDocument();
  });
});
