import i18n from 'i18next';

/**
 * Publish strings live here rather than in src/i18n/locales/*.json so that the
 * feature adds no conflict sites to the thirteen upstream locale files.
 *
 * Two things are needed to keep this out of tree:
 *  - `extract.ignore` in i18next.config.ts, so the CLI does not demand these
 *    keys in every locale file.
 *  - the `PublishTranslations` intersection in src/@types/i18next.d.ts, so
 *    `t('publish.*')` type-checks against the CustomTypeOptions resources.
 */
export const publishResources = {
  panel: {
    title: 'Publish',
  },
};

export type PublishTranslations = {
  publish: typeof publishResources;
};

/** Languages other than `en` are added here as translations arrive. */
const bundles: Record<string, typeof publishResources> = {
  en: publishResources,
};

export const registerPublishResources = () => {
  for (const [language, resources] of Object.entries(bundles)) {
    i18n.addResourceBundle(language, 'translation', { publish: resources }, true, true);
  }
};
