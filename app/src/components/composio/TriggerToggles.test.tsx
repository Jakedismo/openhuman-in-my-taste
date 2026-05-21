import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import TriggerToggles, { activeTriggerSignature, triggerSignature } from './TriggerToggles';

const mockListAvailable = vi.fn();
const mockListTriggers = vi.fn();
const mockEnable = vi.fn();
const mockDisable = vi.fn();

vi.mock('../../lib/composio/composioApi', () => ({
  listAvailableTriggers: (toolkit: string, conn?: string) => mockListAvailable(toolkit, conn),
  listTriggers: (toolkit?: string) => mockListTriggers(toolkit),
  enableTrigger: (conn: string, slug: string, cfg?: Record<string, unknown>) =>
    mockEnable(conn, slug, cfg),
  disableTrigger: (id: string) => mockDisable(id),
}));

beforeEach(() => {
  mockListAvailable.mockReset();
  mockListTriggers.mockReset();
  mockEnable.mockReset();
  mockDisable.mockReset();
});

describe('triggerSignature / activeTriggerSignature', () => {
  it('keys static triggers by uppercase slug', () => {
    expect(triggerSignature('gmail_new', 'static')).toBe('GMAIL_NEW');
  });

  it('keys github triggers by slug + lowercase owner/repo', () => {
    expect(
      triggerSignature('GITHUB_PUSH_EVENT', 'github_repo', { owner: 'Acme', repo: 'API' })
    ).toBe('GITHUB_PUSH_EVENT::acme/api');
  });

  it('falls back to slug when owner/repo are missing on a github_repo entry', () => {
    expect(triggerSignature('GITHUB_PUSH_EVENT', 'github_repo')).toBe('GITHUB_PUSH_EVENT');
  });

  it('matches active trigger signature using triggerConfig.owner/repo', () => {
    expect(
      activeTriggerSignature({
        id: 't1',
        slug: 'github_push_event',
        toolkit: 'github',
        connectionId: 'c1',
        triggerConfig: { owner: 'Acme', repo: 'API' },
      })
    ).toBe('GITHUB_PUSH_EVENT::acme/api');
  });

  it('falls back to slug when triggerConfig has no owner/repo', () => {
    expect(
      activeTriggerSignature({ id: 't2', slug: 'GMAIL_NEW', toolkit: 'gmail', connectionId: 'c1' })
    ).toBe('GMAIL_NEW');
  });
});

describe('<TriggerToggles>', () => {
  it('renders Loading then a list of available triggers', async () => {
    mockListAvailable.mockResolvedValue({
      triggers: [{ slug: 'GMAIL_NEW_GMAIL_MESSAGE', scope: 'static' }],
    });
    mockListTriggers.mockResolvedValue({ triggers: [] });

    render(<TriggerToggles toolkitSlug="gmail" toolkitName="Gmail" connectionId="c1" />);

    expect(screen.getByText('Loading…')).toBeInTheDocument();

    await waitFor(() =>
      expect(screen.getByLabelText(/Enable Gmail New Gmail Message/)).toBeInTheDocument()
    );
    expect(mockListAvailable).toHaveBeenCalledWith('gmail', 'c1');
    expect(mockListTriggers).toHaveBeenCalledWith('gmail');
  });

  it('renders the empty state when no triggers are available', async () => {
    mockListAvailable.mockResolvedValue({ triggers: [] });
    mockListTriggers.mockResolvedValue({ triggers: [] });

    render(<TriggerToggles toolkitSlug="notion" toolkitName="Notion" connectionId="c1" />);

    await waitFor(() =>
      expect(
        screen.getByText('No triggers are currently available for Notion.')
      ).toBeInTheDocument()
    );
  });

  it('shows a load error when available trigger list fails', async () => {
    mockListAvailable.mockRejectedValue(new Error('boom'));
    mockListTriggers.mockResolvedValue({ triggers: [] });

    render(<TriggerToggles toolkitSlug="gmail" toolkitName="Gmail" connectionId="c1" />);

    await waitFor(() =>
      expect(screen.getByText(/Couldn't load triggers: boom/)).toBeInTheDocument()
    );
  });

  it('marks a trigger as enabled when present in active list (matched by signature)', async () => {
    mockListAvailable.mockResolvedValue({
      triggers: [{ slug: 'GMAIL_NEW_GMAIL_MESSAGE', scope: 'static' }],
    });
    mockListTriggers.mockResolvedValue({
      triggers: [
        { id: 't1', slug: 'GMAIL_NEW_GMAIL_MESSAGE', toolkit: 'gmail', connectionId: 'c1' },
      ],
    });

    render(<TriggerToggles toolkitSlug="gmail" toolkitName="Gmail" connectionId="c1" />);

    const sw = await screen.findByLabelText(/Disable Gmail New Gmail Message/);
    expect(sw).toHaveAttribute('aria-checked', 'true');
  });

  it('ignores active triggers attached to a different connection', async () => {
    mockListAvailable.mockResolvedValue({
      triggers: [{ slug: 'GMAIL_NEW_GMAIL_MESSAGE', scope: 'static' }],
    });
    mockListTriggers.mockResolvedValue({
      triggers: [
        { id: 't1', slug: 'GMAIL_NEW_GMAIL_MESSAGE', toolkit: 'gmail', connectionId: 'OTHER' },
      ],
    });

    render(<TriggerToggles toolkitSlug="gmail" toolkitName="Gmail" connectionId="c1" />);

    const sw = await screen.findByLabelText(/Enable Gmail New Gmail Message/);
    expect(sw).toHaveAttribute('aria-checked', 'false');
  });

  it('opens an inline config form for static triggers that require config', async () => {
    // The old behavior was to disable the toggle entirely and show a
    // "Needs configuration" hint, which left direct-mode users with
    // no way to enable triggers like GITHUB_COMMIT_EVENT (owner + repo
    // required). The new behavior keeps the toggle clickable; clicking
    // expands an inline form with one text input per required key.
    mockListAvailable.mockResolvedValue({
      triggers: [{ slug: 'SLACK_NEW_MESSAGE', scope: 'static', requiredConfigKeys: ['channel'] }],
    });
    mockListTriggers.mockResolvedValue({ triggers: [] });

    render(<TriggerToggles toolkitSlug="slack" toolkitName="Slack" connectionId="c1" />);

    const sw = await screen.findByLabelText(/Enable Slack New Message/);
    expect(sw).not.toBeDisabled();
    expect(screen.getByText(/Requires:\s*channel/)).toBeInTheDocument();
    // No config form is open until the user clicks the toggle.
    expect(screen.queryByTestId(/^trigger-config-form-/)).not.toBeInTheDocument();
    expect(mockEnable).not.toHaveBeenCalled();
  });

  it('submits the inline config form with the typed values and flips the toggle on', async () => {
    mockListAvailable.mockResolvedValue({
      triggers: [
        {
          slug: 'GITHUB_COMMIT_EVENT',
          scope: 'static',
          requiredConfigKeys: ['owner', 'repo'],
        },
      ],
    });
    mockListTriggers.mockResolvedValue({ triggers: [] });
    mockEnable.mockResolvedValue({
      triggerId: 'ti_commit_evt',
      slug: 'GITHUB_COMMIT_EVENT',
      connectionId: 'c1',
    });

    render(<TriggerToggles toolkitSlug="github" toolkitName="GitHub" connectionId="c1" />);

    const sw = await screen.findByLabelText(/Enable GITHUB COMMIT EVENT/i);
    // Clicking the toggle opens the inline form rather than calling
    // enableTrigger immediately.
    fireEvent.click(sw);
    const form = await screen.findByTestId('trigger-config-form-GITHUB_COMMIT_EVENT');
    expect(form).toBeInTheDocument();
    expect(mockEnable).not.toHaveBeenCalled();

    // Type values into the two required fields…
    const ownerInput = screen.getByLabelText('owner') as HTMLInputElement;
    const repoInput = screen.getByLabelText('repo') as HTMLInputElement;
    fireEvent.change(ownerInput, { target: { value: 'jruokola' } });
    fireEvent.change(repoInput, { target: { value: 'closedhuman' } });

    // …and submit. enableTrigger gets the trimmed values; the toggle
    // flips to aria-checked=true once the promise resolves.
    fireEvent.click(screen.getByRole('button', { name: /Enable$/ }));
    await waitFor(() => {
      expect(mockEnable).toHaveBeenCalledWith('c1', 'GITHUB_COMMIT_EVENT', {
        owner: 'jruokola',
        repo: 'closedhuman',
      });
    });
    await waitFor(() => {
      expect(sw).toHaveAttribute('aria-checked', 'true');
    });
    // Form closes after successful submission.
    expect(
      screen.queryByTestId('trigger-config-form-GITHUB_COMMIT_EVENT')
    ).not.toBeInTheDocument();
  });

  it('rejects an empty required field with an inline error', async () => {
    mockListAvailable.mockResolvedValue({
      triggers: [
        {
          slug: 'GITHUB_COMMIT_EVENT',
          scope: 'static',
          requiredConfigKeys: ['owner', 'repo'],
        },
      ],
    });
    mockListTriggers.mockResolvedValue({ triggers: [] });

    render(<TriggerToggles toolkitSlug="github" toolkitName="GitHub" connectionId="c1" />);

    const sw = await screen.findByLabelText(/Enable GITHUB COMMIT EVENT/i);
    fireEvent.click(sw);
    await screen.findByTestId('trigger-config-form-GITHUB_COMMIT_EVENT');

    // Only fill owner, leave repo blank.
    fireEvent.change(screen.getByLabelText('owner'), { target: { value: 'jruokola' } });
    fireEvent.click(screen.getByRole('button', { name: /Enable$/ }));

    // No upstream call, error surfaced inline naming the missing field.
    expect(mockEnable).not.toHaveBeenCalled();
    await waitFor(() => {
      expect(screen.getByText(/Fill in:\s*repo/)).toBeInTheDocument();
    });
  });

  it('enables a trigger via enableTrigger and flips the toggle on', async () => {
    mockListAvailable.mockResolvedValue({
      triggers: [
        { slug: 'GMAIL_NEW_GMAIL_MESSAGE', scope: 'static', defaultConfig: { labelIds: 'INBOX' } },
      ],
    });
    mockListTriggers.mockResolvedValue({ triggers: [] });
    mockEnable.mockResolvedValue({
      triggerId: 'ti_1',
      slug: 'GMAIL_NEW_GMAIL_MESSAGE',
      connectionId: 'c1',
    });

    render(<TriggerToggles toolkitSlug="gmail" toolkitName="Gmail" connectionId="c1" />);

    const sw = await screen.findByLabelText(/Enable Gmail New Gmail Message/);
    fireEvent.click(sw);

    await waitFor(() =>
      expect(screen.getByLabelText(/Disable Gmail New Gmail Message/)).toHaveAttribute(
        'aria-checked',
        'true'
      )
    );
    expect(mockEnable).toHaveBeenCalledWith('c1', 'GMAIL_NEW_GMAIL_MESSAGE', { labelIds: 'INBOX' });
  });

  it('renders github_repo entries with owner/repo label and forwards repo as triggerConfig on enable', async () => {
    mockListAvailable.mockResolvedValue({
      triggers: [
        {
          slug: 'GITHUB_PUSH_EVENT',
          scope: 'github_repo',
          repo: { owner: 'acme', repo: 'api' },
          defaultConfig: { owner: 'acme', repo: 'api' },
        },
      ],
    });
    mockListTriggers.mockResolvedValue({ triggers: [] });
    mockEnable.mockResolvedValue({
      triggerId: 'ti_g',
      slug: 'GITHUB_PUSH_EVENT',
      connectionId: 'c1',
    });

    render(<TriggerToggles toolkitSlug="github" toolkitName="GitHub" connectionId="c1" />);

    expect(await screen.findByText('acme/api')).toBeInTheDocument();
    fireEvent.click(screen.getByLabelText(/Enable GitHub Push Event/));

    await waitFor(() => expect(mockEnable).toHaveBeenCalled());
    expect(mockEnable).toHaveBeenCalledWith('c1', 'GITHUB_PUSH_EVENT', {
      owner: 'acme',
      repo: 'api',
    });
  });

  it('disables a trigger via disableTrigger and flips the toggle off', async () => {
    mockListAvailable.mockResolvedValue({
      triggers: [{ slug: 'GMAIL_NEW_GMAIL_MESSAGE', scope: 'static' }],
    });
    mockListTriggers.mockResolvedValue({
      triggers: [
        { id: 't1', slug: 'GMAIL_NEW_GMAIL_MESSAGE', toolkit: 'gmail', connectionId: 'c1' },
      ],
    });
    mockDisable.mockResolvedValue({ deleted: true });

    render(<TriggerToggles toolkitSlug="gmail" toolkitName="Gmail" connectionId="c1" />);

    const sw = await screen.findByLabelText(/Disable Gmail New Gmail Message/);
    fireEvent.click(sw);

    await waitFor(() =>
      expect(screen.getByLabelText(/Enable Gmail New Gmail Message/)).toHaveAttribute(
        'aria-checked',
        'false'
      )
    );
    expect(mockDisable).toHaveBeenCalledWith('t1');
  });

  it('surfaces an error message when enableTrigger fails', async () => {
    mockListAvailable.mockResolvedValue({
      triggers: [{ slug: 'GMAIL_NEW_GMAIL_MESSAGE', scope: 'static' }],
    });
    mockListTriggers.mockResolvedValue({ triggers: [] });
    mockEnable.mockRejectedValue(new Error('upstream 500'));

    render(<TriggerToggles toolkitSlug="gmail" toolkitName="Gmail" connectionId="c1" />);

    fireEvent.click(await screen.findByLabelText(/Enable Gmail New Gmail Message/));

    await waitFor(() =>
      expect(
        screen.getByText(/Enable failed for Gmail New Gmail Message: upstream 500/)
      ).toBeInTheDocument()
    );
  });
});
