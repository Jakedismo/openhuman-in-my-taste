import { render, screen } from '@testing-library/react';
import { MemoryRouter, Route, Routes } from 'react-router-dom';
import { describe, expect, it, vi } from 'vitest';

import ProtectedRoute from '../ProtectedRoute';

const mockUseCoreState = vi.fn();

vi.mock('../../providers/CoreStateProvider', () => ({ useCoreState: () => mockUseCoreState() }));

function renderRoute(routes: React.ReactNode, initialEntries = ['/']) {
  return render(
    <MemoryRouter initialEntries={initialEntries}>
      <Routes>{routes}</Routes>
    </MemoryRouter>
  );
}

describe('ProtectedRoute', () => {
  it('renders a loading screen while bootstrapping', () => {
    mockUseCoreState.mockReturnValue({ isBootstrapping: true });

    renderRoute(
      <Route
        path="/"
        element={
          <ProtectedRoute>
            <div>Protected Content</div>
          </ProtectedRoute>
        }
      />
    );

    expect(screen.queryByText('Protected Content')).not.toBeInTheDocument();
  });

  it('renders children once bootstrapping completes', () => {
    mockUseCoreState.mockReturnValue({ isBootstrapping: false });

    renderRoute(
      <Route
        path="/"
        element={
          <ProtectedRoute>
            <div>Protected Content</div>
          </ProtectedRoute>
        }
      />
    );

    expect(screen.getByText('Protected Content')).toBeInTheDocument();
  });
});
