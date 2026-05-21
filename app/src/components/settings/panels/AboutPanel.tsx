/**
 * About / Updates settings panel.
 *
 * Surfaces the running app version and a link to the GitHub releases page.
 *
 * The auto-update plumbing (`useAppUpdate`, `<AppUpdatePrompt />`,
 * `tauri-plugin-updater`) is intentionally idle in the closedhuman fork:
 * we do not consume upstream tinyhumansai/openhuman releases. The
 * "Check for updates" surface is removed rather than left to error against
 * a disabled plugin. When the fork has its own signed release feed,
 * re-enable `updater.active` in `tauri.conf.json`, re-mount
 * `<AppUpdatePrompt />` in `App.tsx`, and restore the Software Updates
 * card here.
 */
import { useT } from '../../../lib/i18n/I18nContext';
import { APP_VERSION, LATEST_APP_DOWNLOAD_URL } from '../../../utils/config';
import { openUrl } from '../../../utils/openUrl';
import SettingsHeader from '../components/SettingsHeader';
import { useSettingsNavigation } from '../hooks/useSettingsNavigation';

const AboutPanel = () => {
  const { t } = useT();
  const { navigateBack, breadcrumbs } = useSettingsNavigation();

  return (
    <div className="z-10 relative">
      <SettingsHeader
        title={t('settings.about')}
        showBackButton={true}
        onBack={navigateBack}
        breadcrumbs={breadcrumbs}
      />

      <div className="p-4 space-y-4">
        <div className="rounded-xl border border-stone-200 dark:border-neutral-800 bg-white dark:bg-neutral-900 p-4">
          <div className="text-xs text-stone-500 dark:text-neutral-400">
            {t('settings.about.version')}
          </div>
          <div className="mt-1 text-lg font-semibold text-stone-900 dark:text-neutral-100">
            v{APP_VERSION}
          </div>
        </div>

        <div className="rounded-xl border border-stone-200 dark:border-neutral-800 bg-white dark:bg-neutral-900 p-4">
          <div className="text-sm font-medium text-stone-900 dark:text-neutral-100">
            {t('settings.about.releases')}
          </div>
          <p className="mt-1 text-xs text-stone-500 dark:text-neutral-400 leading-relaxed">
            {t('settings.about.releasesDesc')}
          </p>
          <button
            type="button"
            onClick={() => {
              void openUrl(LATEST_APP_DOWNLOAD_URL);
            }}
            className="mt-3 px-3 py-1.5 rounded-lg border border-stone-200 dark:border-neutral-800 text-stone-700 dark:text-neutral-200 hover:bg-stone-100 dark:hover:bg-neutral-800 dark:bg-neutral-800 dark:hover:bg-neutral-800/60 text-xs transition-colors">
            {t('settings.about.openReleases')}
          </button>
        </div>
      </div>
    </div>
  );
};

export default AboutPanel;
