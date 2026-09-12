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
    close: 'Close',
    openPanel: 'Publish album',
    loading: 'Checking your SmugMug connection…',
    retry: 'Try again',
  },
  smugmug: {
    // Named by the backend's AuthChallenge.instructions_key.
    authInstructions:
      'Sign in to SmugMug in your browser and approve access. SmugMug then shows a six-digit code: paste it below.',
    setup: {
      heading: 'Connect SmugMug',
      intro:
        'RapidRAW publishes to SmugMug with an API key that belongs to you, not to RapidRAW. Getting one takes a couple of minutes and is free with any SmugMug account.',
      applyLink: 'Apply for a SmugMug API key',
      applyHint: 'Any application name will do. Once approved, SmugMug shows you a key and a secret.',
      keyLabel: 'API key',
      secretLabel: 'API secret',
      privacy:
        'Your key and secret are kept in your system keychain. They never leave this computer, except to sign requests sent to SmugMug.',
      save: 'Save key',
      saving: 'Saving…',
      cancel: 'Cancel',
    },
    authorise: {
      heading: 'Authorise RapidRAW',
      stepOne: 'Open SmugMug in your browser',
      connect: 'Connect',
      reopen: 'Open again',
      opening: 'Opening…',
      stepTwo: 'Approve access, then paste the six-digit code SmugMug shows you',
      verifierLabel: 'Code from SmugMug',
      verifierPlaceholder: '123456',
      finish: 'Finish connecting',
      finishing: 'Connecting…',
      changeKey: 'Use a different API key',
    },
  },
  connected: {
    heading: 'Account',
    account: 'Connected as {{account}}',
    reconnect: 'Reconnect',
    changeKey: 'Change API key',
  },
  album: {
    heading: 'Album',
    placeholder: 'Choose an album',
    noAlbums: 'Create an album in the library first. Publishing works on albums, not folders.',
    mapping: 'Publishes to “{{name}}” at the top level of your SmugMug site, creating it if needed.',
    empty: 'This album has no photos.',
  },
  settings: {
    heading: 'Export settings',
    current: 'Publishing with your current export settings:',
    quality: '{{quality}}% quality',
    resize: 'fit to {{value}} px',
    fullSize: 'full size',
    watermark: 'watermark',
    openExport: 'Change in the Export panel',
    unsupportedFormat: '{{destination}} accepts {{formats}} only. Choose one of those in the Export panel.',
  },
  preview: {
    heading: 'Changes',
    checking: 'Checking for changes…',
    counts: '{{unchanged}} unchanged, {{update}} to update, {{new}} new',
    unreadable_one: '{{count}} photo could not be read and will be reported as failed.',
    unreadable_other: '{{count}} photos could not be read and will be reported as failed.',
    failed: 'Could not check for changes: {{error}}',
  },
  actions: {
    publish_one: 'Publish {{count}} photo',
    publish_other: 'Publish {{count}} photos',
    upToDate: 'Album is up to date',
    starting: 'Starting…',
    cancel: 'Cancel publishing',
    cancelling: 'Cancelling…',
    done: 'Done',
  },
  progress: {
    heading: 'Publishing',
    count: '{{completed}} of {{total}}',
    preparing: 'Preparing photos…',
    completeHeading: 'Published',
    cancelledHeading: 'Publishing cancelled',
    errorHeading: 'Publishing stopped',
    summary: '{{uploaded}} new, {{updated}} updated, {{skipped}} unchanged',
    failedCount_one: '{{count}} failed',
    failedCount_other: '{{count}} failed',
    ambiguous_one: 'SmugMug did not confirm {{count}} photo. It will be tried again next time you publish.',
    ambiguous_other: 'SmugMug did not confirm {{count}} photos. They will be tried again next time you publish.',
    states: {
      skipped: 'Unchanged',
      uploaded: 'Uploaded',
      updated: 'Updated',
      failed: 'Failed',
      ambiguous: 'Unconfirmed',
    },
  },
  errors: {
    localFile: 'The photo could not be prepared on this computer.',
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
